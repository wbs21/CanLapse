//! 视频合成：把会话缓存里的 `frame_xxxx.png` 批量编码成 H.264 MP4。
//!
//! 编码器是**自编译最小 ffmpeg** 里的 `libopenh264`（静态编在 `avcodec-63.dll` 内），
//! 封装用 libavformat 的 `mp4` muxer，像素转换用 libswscale（BGRA → YUV420P）。
//! 方案与实测依据见 [ADR 0003](../../docs/decisions/0003-video-encoding-ffmpeg-min-openh264.md)。
//!
//! 与原型（spike）的区别 —— 原型只关心"能不能跑通"，这里要能长期稳定运行：
//!
//! * FFI 对象统一 **RAII 释放**，任何错误路径都不会泄漏 context / frame / packet；
//! * 支持**中途取消**（导出面板的【取消合成】），取消时仍然写 trailer，
//!   已编码的部分是**可播放**的，而不是一个损坏文件；
//! * 会话里混有**不同尺寸**的帧时（录制过程中用户改了画布窗口大小），
//!   用 swscale 统一到首帧尺寸，而不是直接失败。

mod ffi;

use std::ffi::CString;
use std::fs;
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::ptr;

use ffi::off::*;
use ffi::*;

use crate::png_io;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    High,
    Medium,
    Low,
}

impl Quality {
    pub fn parse(s: &str) -> Option<Quality> {
        match s.to_ascii_lowercase().as_str() {
            "high" | "高" => Some(Quality::High),
            "medium" | "中" => Some(Quality::Medium),
            "low" | "低" => Some(Quality::Low),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Quality::High => "高",
            Quality::Medium => "中",
            Quality::Low => "低",
        }
    }

    /// 目标码率（bit/s）。
    ///
    /// libopenh264 **不支持 CRF**（那是 x264 的能力），所以统一按
    /// "分辨率 × 帧率 × 每像素比特数" 估算。参考点：1080p24 在"高"档约 7.5 Mbps，
    /// 与常见录屏工具的中高质量预设相当。
    pub fn bitrate_for(self, width: i32, height: i32, fps: f64) -> i64 {
        let bits_per_pixel = match self {
            Quality::High => 0.15,
            Quality::Medium => 0.09,
            Quality::Low => 0.05,
        };
        let raw = width as f64 * height as f64 * fps * bits_per_pixel;
        raw.clamp(300_000.0, 40_000_000.0) as i64
    }
}

#[derive(Clone, Debug)]
pub struct EncodeOptions {
    /// 导出播放帧率（5/10/15/20/24）—— 与录制帧密度完全无关，这是"缩时"的核心
    pub fps: f64,
    pub quality: Quality,
    pub output: PathBuf,
}

pub struct EncodeStats {
    pub frames: u32,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
    pub seconds: f64,
}

/// 进度回调的返回值：让 UI 能在合成中途取消
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Progress {
    Continue,
    Cancel,
}

pub enum EncodeOutcome {
    Done(EncodeStats),
    /// 用户取消：文件已经写成可播放的部分，统计为已编码帧数
    Cancelled(EncodeStats),
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

fn av_err(code: c_int) -> String {
    let mut buf = [0 as c_char; 256];
    unsafe { av_strerror(code, buf.as_mut_ptr(), buf.len()) };
    unsafe { cstr(buf.as_ptr()) }
}

fn ck(what: &str, r: c_int) -> Result<c_int, String> {
    if r < 0 {
        Err(format!("{what} 失败: {r} ({})", av_err(r)))
    } else {
        Ok(r)
    }
}

unsafe fn set_opt_str(obj: *mut c_void, name: &str, value: &str) {
    if obj.is_null() {
        return;
    }
    if let (Ok(n), Ok(v)) = (CString::new(name), CString::new(value)) {
        av_opt_set(obj, n.as_ptr(), v.as_ptr(), 0);
    }
}

unsafe fn set_opt_int(obj: *mut c_void, name: &str, value: i64) {
    if obj.is_null() {
        return;
    }
    if let Ok(n) = CString::new(name) {
        av_opt_set_int(obj, n.as_ptr(), value, 0);
    }
}

fn gcd(a: i32, b: i32) -> i32 {
    if b == 0 {
        a.abs().max(1)
    } else {
        gcd(b, a % b)
    }
}

/// 帧率 -> 帧时长的有理数（`time_base`）。
/// 规格里的 5/10/15/20/24 都是整数，走精确的 `1/n`；非整数帧率用千分之一精度表示。
fn fps_rational(fps: f64) -> AVRational {
    let rounded = fps.round();
    if (fps - rounded).abs() < 1e-9 && rounded >= 1.0 {
        return AVRational::new(1, rounded as i32);
    }
    let num = (fps * 1000.0).round() as i32;
    let den = 1000;
    let g = gcd(num, den);
    AVRational::new(den / g, num / g)
}

/// RGBA -> BGRA（就地交换 R 与 B）。swscale 吃的是 BGRA。
fn rgba_to_bgra(mut px: Vec<u8>) -> Vec<u8> {
    for c in px.chunks_exact_mut(4) {
        c.swap(0, 2);
    }
    px
}

/// 收集会话目录里的 `frame_*.png`，**按帧号排序**。
///
/// 不能按文件名字符串排序：序号超过 9999 后宽度会变成 5 位，
/// 字符串比较会把 `frame_10000.png` 排到 `frame_9999.png` 前面。
fn collect_frames(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("读取 {} 失败: {e}", dir.display()))?;
    let mut indexed: Vec<(u32, PathBuf)> = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with("frame_") || !name.ends_with(".png") {
            continue;
        }
        let digits = &name["frame_".len()..name.len() - ".png".len()];
        let idx = match digits.parse::<u32>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        indexed.push((idx, e.path()));
    }
    indexed.sort_by_key(|(i, _)| *i);
    Ok(indexed.into_iter().map(|(_, p)| p).collect())
}

// ---------------------------------------------------------------------------
// 编码器 + 封装器（RAII）
// ---------------------------------------------------------------------------

/// 持有全部需要释放的 FFI 对象；任何一步失败都直接 `return Err`，由 `Drop` 收尾。
struct Muxer {
    oc: *mut AVFormatContext,
    pb_slot: *mut *mut AVIOContext,
    ctx: *mut AVCodecContext,
    frame: *mut AVFrame,
    pkt: *mut AVPacket,
    sws: *mut SwsContext,
    sws_src: (i32, i32),
    enc_w: i32,
    enc_h: i32,
    src_tb: AVRational,
    dst_tb: AVRational,
    header_written: bool,
    trailer_written: bool,
    total_bytes: u64,
    packets: usize,
}

impl Muxer {
    fn empty() -> Self {
        Self {
            oc: ptr::null_mut(),
            pb_slot: ptr::null_mut(),
            ctx: ptr::null_mut(),
            frame: ptr::null_mut(),
            pkt: ptr::null_mut(),
            sws: ptr::null_mut(),
            sws_src: (0, 0),
            enc_w: 0,
            enc_h: 0,
            src_tb: AVRational::new(1, 1),
            dst_tb: AVRational::new(1, 1),
            header_written: false,
            trailer_written: false,
            total_bytes: 0,
            packets: 0,
        }
    }

    unsafe fn open(
        output: &Path,
        width: i32,
        height: i32,
        fps: f64,
        bitrate: i64,
    ) -> Result<Muxer, String> {
        // 日志级别：默认只报错；排查时可用 CANLAPSE_AV_LOG=info
        let level = match std::env::var("CANLAPSE_AV_LOG").unwrap_or_default().as_str() {
            "info" => AV_LOG_INFO,
            "warning" | "warn" => AV_LOG_WARNING,
            _ => AV_LOG_ERROR,
        };
        av_log_set_level(level);

        let mut m = Muxer::empty();
        m.enc_w = width;
        m.enc_h = height;
        m.src_tb = fps_rational(fps);

        let codec_name = CString::new("libopenh264").unwrap();
        let codec = avcodec_find_encoder_by_name(codec_name.as_ptr());
        if codec.is_null() {
            return Err(
                "这套 ffmpeg 构建里找不到 libopenh264 编码器（请检查 CANLAPSE_FFMPEG_MIN 指向的是不是那份自编译最小构建）"
                    .into(),
            );
        }

        m.ctx = avcodec_alloc_context3(codec);
        if m.ctx.is_null() {
            return Err("avcodec_alloc_context3 失败".into());
        }

        // ---- 没有 AVOption 的字段，只能按实测偏移直接写（见 ffi.rs）----
        set_i32(m.ctx, CC_WIDTH, width);
        set_i32(m.ctx, CC_HEIGHT, height);
        set_i32(m.ctx, CC_PIX_FMT, AV_PIX_FMT_YUV420P);
        set_rat(m.ctx, CC_TIME_BASE, m.src_tb);
        set_rat(
            m.ctx,
            CC_FRAMERATE,
            AVRational::new(m.src_tb.den, m.src_tb.num),
        );
        set_rat(m.ctx, CC_SAMPLE_ASPECT_RATIO, AVRational::new(1, 1));
        // 约 1 秒一个关键帧：太小则文件大，太大则拖动定位不准
        set_i32(m.ctx, CC_GOP_SIZE, (m.src_tb.den / m.src_tb.num).max(1));
        set_i32(m.ctx, CC_MAX_B_FRAMES, 0);
        set_i64(m.ctx, CC_BIT_RATE, bitrate);
        set_i32(m.ctx, CC_RC_MAX_RATE, bitrate as c_int);
        set_i32(m.ctx, CC_RC_BUFFER_SIZE, (bitrate / 2) as c_int);
        set_i32(m.ctx, CC_THREAD_COUNT, 1);
        // GLOBAL_HEADER 必须在 avcodec_open2 之前设，否则 MP4 里没有 avcC
        let flags = get_i32(m.ctx, CC_FLAGS);
        set_i32(m.ctx, CC_FLAGS, flags | AV_CODEC_FLAG_GLOBAL_HEADER);

        // 包装器的私有选项要写在 ctx->priv_data 上：
        // 用 AV_OPT_SEARCH_CHILDREN 从 ctx 设会返回 AVERROR_OPTION_NOT_FOUND（实测）。
        let priv_data = get_ptr::<c_void>(m.ctx, CC_PRIV_DATA);
        // profile 用 high：ADR 0003 实测它体积更小、质量更好，
        // 且能避开 constrained_baseline 触发的无害警告。
        set_opt_str(priv_data, "profile", "high");
        set_opt_str(priv_data, "rc_mode", "bitrate");
        set_opt_int(priv_data, "allow_skip_frames", 0);

        ck("avcodec_open2", avcodec_open2(m.ctx, codec, ptr::null_mut()))?;

        // ---- 可复用的编码侧 YUV420P 帧 ----
        m.frame = av_frame_alloc();
        if m.frame.is_null() {
            return Err("av_frame_alloc 失败".into());
        }
        set_i32(m.frame, FRAME_FORMAT, AV_PIX_FMT_YUV420P);
        set_i32(m.frame, FRAME_WIDTH, width);
        set_i32(m.frame, FRAME_HEIGHT, height);
        ck("av_frame_get_buffer", av_frame_get_buffer(m.frame, 32))?;

        m.pkt = av_packet_alloc();
        if m.pkt.is_null() {
            return Err("av_packet_alloc 失败".into());
        }

        // ---- 输出上下文 ----
        let fmt = CString::new("mp4").unwrap();
        let path_str = output.to_string_lossy().replace('/', "\\");
        let fname = CString::new(path_str).map_err(|e| format!("输出路径非法: {e}"))?;
        ck(
            "avformat_alloc_output_context2",
            avformat_alloc_output_context2(&mut m.oc, ptr::null(), fmt.as_ptr(), fname.as_ptr()),
        )?;

        let st = avformat_new_stream(m.oc, ptr::null());
        if st.is_null() {
            return Err("avformat_new_stream 失败".into());
        }
        let codecpar = get_ptr::<AVCodecParameters>(st, ST_CODECPAR);
        ck(
            "avcodec_parameters_from_context",
            avcodec_parameters_from_context(codecpar, m.ctx),
        )?;

        set_rat(st, ST_TIME_BASE, AVRational::new(1, 1000));
        let fr = AVRational::new(m.src_tb.den, m.src_tb.num);
        set_rat(st, ST_AVG_FRAME_RATE, fr);
        set_rat(st, ST_R_FRAME_RATE, fr);

        m.pb_slot = (m.oc as *mut u8).add(FC_PB) as *mut *mut AVIOContext;
        ck(
            "avio_open",
            avio_open(m.pb_slot, fname.as_ptr(), AVIO_FLAG_WRITE),
        )?;
        ck(
            "avformat_write_header",
            avformat_write_header(m.oc, ptr::null_mut()),
        )?;
        m.header_written = true;

        // ★ mov/mp4 muxer 会在 avformat_write_header 里**改写** st->time_base
        //   （实测：请求 1/1000，实际选 1/16000）。必须读回它选定的值来换算时间戳，
        //   否则时长会差 16 倍。
        m.dst_tb = get_rat(st, ST_TIME_BASE);
        if m.dst_tb.num == 0 || m.dst_tb.den == 0 {
            m.dst_tb = AVRational::new(1, 1000);
        }
        Ok(m)
    }

    unsafe fn drain_packets(&mut self) -> Result<(), String> {
        loop {
            let r = avcodec_receive_packet(self.ctx, self.pkt);
            if r == AVERROR_EAGAIN || r == AVERROR_EOF {
                break;
            }
            if r < 0 {
                return Err(format!("avcodec_receive_packet 失败: {r} ({})", av_err(r)));
            }
            self.total_bytes += get_i32(self.pkt, PKT_SIZE).max(0) as u64;
            set_i32(self.pkt, PKT_STREAM_INDEX, 0);
            av_packet_rescale_ts(self.pkt, self.src_tb, self.dst_tb);
            ck(
                "av_interleaved_write_frame",
                av_interleaved_write_frame(self.oc, self.pkt),
            )?;
            av_packet_unref(self.pkt);
            self.packets += 1;
        }
        Ok(())
    }

    /// 写一帧。`src_w`/`src_h` 是**用于缩放的**源尺寸（已裁成偶数），
    /// `stride_bytes` 是源缓冲真实的行跨度（源宽为奇数时它比 `src_w * 4` 大）。
    unsafe fn write_frame(
        &mut self,
        bgra: &[u8],
        stride_bytes: i32,
        src_w: i32,
        src_h: i32,
        pts: i64,
    ) -> Result<(), String> {
        if self.sws.is_null() || self.sws_src != (src_w, src_h) {
            if !self.sws.is_null() {
                sws_freeContext(self.sws);
                self.sws = ptr::null_mut();
            }
            self.sws = sws_getContext(
                src_w,
                src_h,
                AV_PIX_FMT_BGRA,
                self.enc_w,
                self.enc_h,
                AV_PIX_FMT_YUV420P,
                SWS_BILINEAR,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            );
            if self.sws.is_null() {
                return Err(format!(
                    "sws_getContext({src_w}x{src_h} BGRA -> {}x{} YUV420P) 失败",
                    self.enc_w, self.enc_h
                ));
            }
            self.sws_src = (src_w, src_h);
        }

        ck("av_frame_make_writable", av_frame_make_writable(self.frame))?;

        let src_planes: [*const u8; 4] = [bgra.as_ptr(), ptr::null(), ptr::null(), ptr::null()];
        let src_strides: [c_int; 4] = [stride_bytes, 0, 0, 0];
        let dst_planes: [*mut u8; 4] = [
            arr_ptr(self.frame, FRAME_DATA, 0),
            arr_ptr(self.frame, FRAME_DATA, 1),
            arr_ptr(self.frame, FRAME_DATA, 2),
            arr_ptr(self.frame, FRAME_DATA, 3),
        ];
        let dst_strides: [c_int; 4] = [
            arr_i32(self.frame, FRAME_LINESIZE, 0),
            arr_i32(self.frame, FRAME_LINESIZE, 1),
            arr_i32(self.frame, FRAME_LINESIZE, 2),
            arr_i32(self.frame, FRAME_LINESIZE, 3),
        ];
        let rows = sws_scale(
            self.sws,
            src_planes.as_ptr(),
            src_strides.as_ptr(),
            0,
            src_h,
            dst_planes.as_ptr(),
            dst_strides.as_ptr(),
        );
        if rows != self.enc_h {
            return Err(format!("sws_scale 只转换了 {rows} 行，期望 {}", self.enc_h));
        }

        set_i64(self.frame, FRAME_PTS, pts);
        ck("avcodec_send_frame", avcodec_send_frame(self.ctx, self.frame))?;
        self.drain_packets()
    }

    /// 冲刷编码器并写 trailer。**取消时也要调用**，这样已编码的部分才能播放。
    unsafe fn finish(&mut self) -> Result<(), String> {
        if self.header_written && !self.trailer_written {
            ck(
                "avcodec_send_frame(NULL) [flush]",
                avcodec_send_frame(self.ctx, ptr::null()),
            )?;
            self.drain_packets()?;
            ck("av_write_trailer", av_write_trailer(self.oc))?;
            self.trailer_written = true;
        }
        Ok(())
    }
}

impl Drop for Muxer {
    fn drop(&mut self) {
        unsafe {
            // 顺序有讲究：先关 IO 再释放 oc（pb_slot 指向 oc 内部）
            if !self.pkt.is_null() {
                av_packet_free(&mut self.pkt);
            }
            if !self.frame.is_null() {
                av_frame_free(&mut self.frame);
            }
            if !self.sws.is_null() {
                sws_freeContext(self.sws);
                self.sws = ptr::null_mut();
            }
            if !self.ctx.is_null() {
                avcodec_free_context(&mut self.ctx);
            }
            if !self.pb_slot.is_null() && !(*self.pb_slot).is_null() {
                avio_closep(self.pb_slot);
            }
            if !self.oc.is_null() {
                avformat_free_context(self.oc);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 对外接口
// ---------------------------------------------------------------------------

/// 这套构建里有没有可用的 `libopenh264` 编码器（给自检命令用）
pub fn encoder_available() -> bool {
    unsafe {
        let n = CString::new("libopenh264").unwrap();
        !avcodec_find_encoder_by_name(n.as_ptr()).is_null()
    }
}

/// 底层 ffmpeg 构建信息（给自检命令用）
pub fn backend_info() -> String {
    unsafe { cstr(avcodec_configuration()) }
}

/// 把会话目录里的 PNG 帧合成 MP4。
///
/// * 输出尺寸取**首帧**尺寸（宽高向下取偶数，libopenh264 要求偶数）；
/// * `progress(已编码帧数, 总帧数)` 返回 [`Progress::Cancel`] 即可取消，
///   取消后仍会写 trailer，产出文件是**可播放的部分视频**；
/// * 单帧读不出来直接报错（缓存被外部删改属于异常，不该悄悄产出缺帧的视频）。
pub fn encode_session(
    session_dir: &Path,
    opts: &EncodeOptions,
    progress: &mut dyn FnMut(u32, u32) -> Progress,
) -> Result<EncodeOutcome, String> {
    if !(1.0..=120.0).contains(&opts.fps) {
        return Err(format!("导出帧率不合理: {}", opts.fps));
    }
    let frames = collect_frames(session_dir)?;
    if frames.is_empty() {
        return Err(format!(
            "{} 里没有 frame_*.png 帧，无法合成",
            session_dir.display()
        ));
    }

    let (first_w, first_h, _) = png_io::read_png_rgba(&frames[0])?;
    let (first_w, first_h) = (first_w as i32, first_h as i32);
    let enc_w = first_w & !1;
    let enc_h = first_h & !1;
    if enc_w < 2 || enc_h < 2 {
        return Err(format!("首帧尺寸异常: {first_w}x{first_h}"));
    }

    if let Some(parent) = opts.output.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("创建输出目录 {} 失败: {e}", parent.display()))?;
        }
    }

    let bitrate = opts.quality.bitrate_for(enc_w, enc_h, opts.fps);
    let total = frames.len() as u32;
    let mut written: u32 = 0;
    let mut cancelled = false;
    let mut size_changes = 0u32;

    {
        let mut muxer = unsafe { Muxer::open(&opts.output, enc_w, enc_h, opts.fps, bitrate)? };

        for (i, path) in frames.iter().enumerate() {
            let (fw, fh, rgba) = png_io::read_png_rgba(path)
                .map_err(|e| format!("读取第 {i} 帧失败（{}）: {e}", path.display()))?;
            let (fw, fh) = (fw as i32, fh as i32);
            if fw != first_w || fh != first_h {
                size_changes += 1;
            }
            // 源尺寸裁成偶数：源宽为奇数时靠 stride 只读前 (fw-1) 列，属于裁剪而非缩放
            let sw = fw & !1;
            let sh = fh & !1;
            if sw < 2 || sh < 2 {
                return Err(format!("第 {i} 帧尺寸异常: {fw}x{fh}"));
            }
            let bgra = rgba_to_bgra(rgba);
            unsafe { muxer.write_frame(&bgra, fw * 4, sw, sh, i as i64)? };
            written += 1;
            if progress(written, total) == Progress::Cancel {
                cancelled = true;
                break;
            }
        }

        unsafe { muxer.finish()? };
    }

    if size_changes > 0 {
        eprintln!(
            "提示: 有 {size_changes} 帧与首帧尺寸不同（录制过程中画布窗口尺寸变化过），已统一缩放到 {enc_w}x{enc_h}。"
        );
    }

    let bytes = fs::metadata(&opts.output).map(|m| m.len()).unwrap_or(0);
    let stats = EncodeStats {
        frames: written,
        bytes,
        width: enc_w as u32,
        height: enc_h as u32,
        seconds: written as f64 / opts.fps,
    };
    Ok(if cancelled {
        EncodeOutcome::Cancelled(stats)
    } else {
        EncodeOutcome::Done(stats)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 画质档位解析支持中英文() {
        assert_eq!(Quality::parse("high"), Some(Quality::High));
        assert_eq!(Quality::parse("高"), Some(Quality::High));
        assert_eq!(Quality::parse("Medium"), Some(Quality::Medium));
        assert_eq!(Quality::parse("低"), Some(Quality::Low));
        assert_eq!(Quality::parse("ultra"), None);
    }

    #[test]
    fn 码率随画质与分辨率单调() {
        let (w, h, fps) = (1920, 1080, 24.0);
        let hi = Quality::High.bitrate_for(w, h, fps);
        let mid = Quality::Medium.bitrate_for(w, h, fps);
        let lo = Quality::Low.bitrate_for(w, h, fps);
        assert!(hi > mid && mid > lo, "{hi} {mid} {lo}");
        // 参考点：1080p24 高质量应落在 Mbps 量级的合理区间
        assert!((4_000_000..12_000_000).contains(&hi), "hi={hi}");
        // 极小分辨率有下限，避免码率低到无法编码
        assert!(Quality::Low.bitrate_for(64, 64, 5.0) >= 300_000);
    }

    #[test]
    fn 帧率有理数转换正确() {
        let tb = fps_rational(24.0);
        assert_eq!((tb.num, tb.den), (1, 24));
        let tb = fps_rational(12.5);
        // time_base = 2/25，即帧时长 0.08 秒
        assert_eq!((tb.num, tb.den), (2, 25));
    }

    #[test]
    fn 帧号排序不按字符串() {
        // 文件名排序会把 frame_10000.png 排到 frame_9999.png 之前，必须按帧号
        let dir = std::env::temp_dir().join(format!("canlapse-frames-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for n in [0u32, 2, 10, 9999, 10000] {
            fs::write(dir.join(format!("frame_{n:04}.png")), b"x").unwrap();
        }
        fs::write(dir.join("markers.json"), b"{}").unwrap();
        let got: Vec<String> = collect_frames(&dir)
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            got,
            vec![
                "frame_0000.png",
                "frame_0002.png",
                "frame_0010.png",
                "frame_9999.png",
                "frame_10000.png"
            ]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rgba转bgra只交换红蓝() {
        let src = vec![1u8, 2, 3, 255, 10, 20, 30, 255];
        assert_eq!(rgba_to_bgra(src), vec![3u8, 2, 1, 255, 30, 20, 10, 255]);
    }
}
