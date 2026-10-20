//! 手写 FFI：自编译最小 ffmpeg（`D:\CapLite\ffmpeg-min`）的 libav* 绑定。
//! FFmpeg n9.0：libavcodec 63.1.102 / libavformat 63.1.102 / libavutil 61.1.102 /
//! libswscale 10.1.102。
//!
//! **来源**：本文件由已通过验证的原型 `spikes/encode-mp4/src/ffi.rs`
//! **逐字节复制**而来（复制后仅改动了本段注释），未做任何重新推导。
//! 原型的三重验证（MP4 box 解析 + 真实解码回读 r=0.99992 + `ffprobe -count_frames`）
//! 见 `spikes/encode-mp4/FINDINGS.md`。
//!
//! ## 为什么手写而不用 `ffmpeg-sys-next` + bindgen
//!
//! `ffmpeg-sys-next` 会跑 bindgen，而 bindgen 需要 `libclang.dll`。
//! 本机离线且各处都没有 libclang（实测三段失败，见 FINDINGS.md 问题 2）。
//!
//! ## [`off`] 里的偏移量为什么可信
//!
//! 它们是**实测**的，不是猜的。`_probe/ffprobe_min.c` 用**同一套头文件**配合
//! MSVC `cl.exe` 编译，打印出这里用到的每个字段的 `offsetof()` 与 `sizeof()`；
//! 原始输出在 `_probe/probe_out.txt`。MSVC `cl.exe` 与
//! `rustc --target x86_64-pc-windows-msvc` 使用同一套 ABI，数值可直接套用。
//!
//! 只建模真正用到的字段；所有对象都保持不透明（`*mut c_void`），
//! 通过显式偏移访问器读写，因此不需要猜任何大型 `#[repr(C)]` 结构体布局。
//!
//! ⚠️ 偏移量随 ffmpeg 版本失效：升级 `ffmpeg-min` 必须重跑探针并更新 [`off`]。

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use std::os::raw::{c_char, c_int, c_void};

pub type AVCodec = c_void;
pub type AVCodecContext = c_void;
pub type AVCodecParameters = c_void;
pub type AVFormatContext = c_void;
pub type AVFrame = c_void;
pub type AVIOContext = c_void;
pub type AVOutputFormat = c_void;
pub type AVPacket = c_void;
pub type AVStream = c_void;
pub type SwsContext = c_void;

/// `AVRational` — a plain pair of `int`, stable across every FFmpeg version.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct AVRational {
    pub num: c_int,
    pub den: c_int,
}

impl AVRational {
    pub const fn new(num: c_int, den: c_int) -> Self {
        Self { num, den }
    }
}

// ---------------------------------------------------------------------------
// Constants, taken from the headers under D:\CapLite\ffmpeg-min\include
// ---------------------------------------------------------------------------

/// `AVERROR_EOF` = `FFERRTAG('E','O','F',' ')` = `-(int)MKTAG(...)`.
pub const AVERROR_EOF: c_int = -541_478_725;
/// `AVERROR(EAGAIN)`; `EAGAIN == 11` on the MSVC CRT (`EDOM > 0` branch of error.h).
pub const AVERROR_EAGAIN: c_int = -11;

pub const AVIO_FLAG_WRITE: c_int = 2;
pub const AVMEDIA_TYPE_VIDEO: c_int = 0;
pub const AV_PIX_FMT_YUV420P: c_int = 0;
pub const AV_PIX_FMT_BGRA: c_int = 28;
pub const SWS_BILINEAR: c_int = 2;
pub const SWS_POINT: c_int = 0x10;
pub const AV_CODEC_FLAG_GLOBAL_HEADER: c_int = 0x0040_0000;
pub const AV_CODEC_ID_H264: c_int = 27;
pub const AV_OPT_SEARCH_CHILDREN: c_int = 0x0002;

pub const AV_LOG_ERROR: c_int = 16;
pub const AV_LOG_WARNING: c_int = 24;
pub const AV_LOG_INFO: c_int = 32;

// ---------------------------------------------------------------------------
// Field offsets measured by _probe/ffprobe_min.c (MSVC x64 ABI)
// ---------------------------------------------------------------------------

pub mod off {
    // ---- AVCodec (sizeof 56) ----
    pub const CODEC_NAME: usize = 0;
    pub const CODEC_LONG_NAME: usize = 8;
    pub const CODEC_TYPE: usize = 16;
    pub const CODEC_ID: usize = 20;
    pub const CODEC_CAPABILITIES: usize = 24;
    pub const CODEC_PRIV_CLASS: usize = 32;
    pub const CODEC_WRAPPER_NAME: usize = 48;

    // ---- AVCodecContext (sizeof 864) ----
    pub const CC_CODEC_TYPE: usize = 12;
    pub const CC_CODEC_ID: usize = 24;
    pub const CC_PRIV_DATA: usize = 32;
    pub const CC_BIT_RATE: usize = 56;
    pub const CC_FLAGS: usize = 64;
    pub const CC_EXTRA_DATA: usize = 72;
    pub const CC_EXTRA_DATA_SIZE: usize = 80;
    pub const CC_TIME_BASE: usize = 84;
    pub const CC_FRAMERATE: usize = 100;
    pub const CC_DELAY: usize = 108;
    pub const CC_WIDTH: usize = 112;
    pub const CC_HEIGHT: usize = 116;
    pub const CC_SAMPLE_ASPECT_RATIO: usize = 128;
    pub const CC_PIX_FMT: usize = 136;
    pub const CC_SW_PIX_FMT: usize = 140;
    pub const CC_COLOR_RANGE: usize = 156;
    pub const CC_REFS: usize = 168;
    pub const CC_MAX_B_FRAMES: usize = 200;
    pub const CC_GOP_SIZE: usize = 332;
    pub const CC_QMIN: usize = 436;
    pub const CC_QMAX: usize = 440;
    pub const CC_RC_BUFFER_SIZE: usize = 448;
    pub const CC_RC_MAX_RATE: usize = 464;
    pub const CC_THREAD_COUNT: usize = 656;
    pub const CC_PROFILE: usize = 688;
    pub const CC_LEVEL: usize = 692;

    // ---- AVFrame (sizeof 424) ----
    pub const FRAME_DATA: usize = 0; // uint8_t *data[8]
    pub const FRAME_LINESIZE: usize = 64; // int linesize[8]
    pub const FRAME_EXTENDED_DATA: usize = 96;
    pub const FRAME_WIDTH: usize = 104;
    pub const FRAME_HEIGHT: usize = 108;
    pub const FRAME_FORMAT: usize = 116;
    pub const FRAME_SAMPLE_ASPECT_RATIO: usize = 124;
    pub const FRAME_PTS: usize = 136;
    pub const FRAME_COLOR_RANGE: usize = 280;

    // ---- AVPacket (sizeof 104) ----
    pub const PKT_PTS: usize = 8;
    pub const PKT_DTS: usize = 16;
    pub const PKT_DATA: usize = 24;
    pub const PKT_SIZE: usize = 32;
    pub const PKT_STREAM_INDEX: usize = 36;
    pub const PKT_FLAGS: usize = 40;
    pub const PKT_DURATION: usize = 64;

    // ---- AVStream (sizeof 216) ----
    pub const ST_INDEX: usize = 8;
    pub const ST_CODECPAR: usize = 16;
    pub const ST_TIME_BASE: usize = 32;
    pub const ST_START_TIME: usize = 40;
    pub const ST_DURATION: usize = 48;
    pub const ST_NB_FRAMES: usize = 56;
    pub const ST_AVG_FRAME_RATE: usize = 88;
    pub const ST_R_FRAME_RATE: usize = 204;

    // ---- AVCodecParameters (sizeof 184) ----
    pub const CP_CODEC_TYPE: usize = 0;
    pub const CP_CODEC_ID: usize = 4;
    pub const CP_EXTRA_DATA: usize = 16;
    pub const CP_EXTRA_DATA_SIZE: usize = 24;
    pub const CP_FORMAT: usize = 44;
    pub const CP_BIT_RATE: usize = 48;
    pub const CP_WIDTH: usize = 72;
    pub const CP_HEIGHT: usize = 76;
    pub const CP_FRAMERATE: usize = 88;

    // ---- AVFormatContext (sizeof 480) ----
    pub const FC_OFORMAT: usize = 16;
    pub const FC_PB: usize = 32;
    pub const FC_NB_STREAMS: usize = 44;
    pub const FC_STREAMS: usize = 48;
    pub const FC_DURATION: usize = 104;
}

// ---------------------------------------------------------------------------
// Offset accessors
// ---------------------------------------------------------------------------

#[inline]
pub unsafe fn get_i32(base: *const c_void, off: usize) -> c_int {
    std::ptr::read_unaligned((base as *const u8).add(off) as *const c_int)
}

#[inline]
pub unsafe fn set_i32(base: *mut c_void, off: usize, v: c_int) {
    std::ptr::write_unaligned((base as *mut u8).add(off) as *mut c_int, v)
}

#[inline]
pub unsafe fn get_i64(base: *const c_void, off: usize) -> i64 {
    std::ptr::read_unaligned((base as *const u8).add(off) as *const i64)
}

#[inline]
pub unsafe fn set_i64(base: *mut c_void, off: usize, v: i64) {
    std::ptr::write_unaligned((base as *mut u8).add(off) as *mut i64, v)
}

#[inline]
pub unsafe fn get_ptr<T>(base: *const c_void, off: usize) -> *mut T {
    std::ptr::read_unaligned((base as *const u8).add(off) as *const *mut T)
}

#[inline]
pub unsafe fn set_ptr<T>(base: *mut c_void, off: usize, v: *mut T) {
    std::ptr::write_unaligned((base as *mut u8).add(off) as *mut *mut T, v)
}

/// Read element `idx` of an inline pointer array (e.g. `AVFrame::data[8]`).
#[inline]
pub unsafe fn arr_ptr<T>(base: *const c_void, off: usize, idx: usize) -> *mut T {
    std::ptr::read_unaligned((base as *const u8).add(off + idx * 8) as *const *mut T)
}

/// Read element `idx` of an inline `int` array (e.g. `AVFrame::linesize[8]`).
#[inline]
pub unsafe fn arr_i32(base: *const c_void, off: usize, idx: usize) -> c_int {
    std::ptr::read_unaligned((base as *const u8).add(off + idx * 4) as *const c_int)
}

#[inline]
pub unsafe fn get_rat(base: *const c_void, off: usize) -> AVRational {
    AVRational {
        num: get_i32(base, off),
        den: get_i32(base, off + 4),
    }
}

#[inline]
pub unsafe fn set_rat(base: *mut c_void, off: usize, v: AVRational) {
    set_i32(base, off, v.num);
    set_i32(base, off + 4, v.den);
}

/// Read a NUL-terminated C string that may be null.
pub unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// libavutil
// ---------------------------------------------------------------------------

extern "C" {
    pub fn av_log_set_level(level: c_int);
    pub fn av_strerror(errnum: c_int, errbuf: *mut c_char, errbuf_size: usize) -> c_int;
    pub fn avcodec_version() -> u32;
    pub fn avutil_version() -> u32;
    pub fn avutil_configuration() -> *const c_char;
    pub fn avcodec_configuration() -> *const c_char;
    pub fn avformat_configuration() -> *const c_char;

    pub fn av_frame_alloc() -> *mut AVFrame;
    pub fn av_frame_free(frame: *mut *mut AVFrame);
    pub fn av_frame_unref(frame: *mut AVFrame);
    pub fn av_frame_get_buffer(frame: *mut AVFrame, align: c_int) -> c_int;
    pub fn av_frame_make_writable(frame: *mut AVFrame) -> c_int;

    pub fn av_rescale_q(a: i64, bq: AVRational, cq: AVRational) -> i64;

    pub fn av_opt_set(
        obj: *mut c_void,
        name: *const c_char,
        val: *const c_char,
        flags: c_int,
    ) -> c_int;
    pub fn av_opt_set_int(obj: *mut c_void, name: *const c_char, val: i64, flags: c_int) -> c_int;
}

// ---------------------------------------------------------------------------
// libavcodec
// ---------------------------------------------------------------------------

extern "C" {
    pub fn av_codec_iterate(opaque: *mut *mut c_void) -> *const AVCodec;
    pub fn av_codec_is_encoder(codec: *const AVCodec) -> c_int;
    pub fn avcodec_find_encoder_by_name(name: *const c_char) -> *const AVCodec;
    pub fn avcodec_find_decoder_by_name(name: *const c_char) -> *const AVCodec;
    pub fn avcodec_find_decoder(id: c_int) -> *const AVCodec;

    pub fn avcodec_alloc_context3(codec: *const AVCodec) -> *mut AVCodecContext;
    pub fn avcodec_free_context(ctx: *mut *mut AVCodecContext);
    pub fn avcodec_open2(
        ctx: *mut AVCodecContext,
        codec: *const AVCodec,
        options: *mut *mut c_void,
    ) -> c_int;

    pub fn avcodec_send_frame(ctx: *mut AVCodecContext, frame: *const AVFrame) -> c_int;
    pub fn avcodec_receive_packet(ctx: *mut AVCodecContext, pkt: *mut AVPacket) -> c_int;
    pub fn avcodec_send_packet(ctx: *mut AVCodecContext, pkt: *const AVPacket) -> c_int;
    pub fn avcodec_receive_frame(ctx: *mut AVCodecContext, frame: *mut AVFrame) -> c_int;

    pub fn avcodec_parameters_from_context(
        par: *mut AVCodecParameters,
        ctx: *const AVCodecContext,
    ) -> c_int;
    pub fn avcodec_parameters_to_context(
        ctx: *mut AVCodecContext,
        par: *const AVCodecParameters,
    ) -> c_int;
    pub fn avcodec_parameters_alloc() -> *mut AVCodecParameters;
    pub fn avcodec_parameters_free(par: *mut *mut AVCodecParameters);

    pub fn av_packet_alloc() -> *mut AVPacket;
    pub fn av_packet_free(pkt: *mut *mut AVPacket);
    pub fn av_packet_unref(pkt: *mut AVPacket);
    pub fn av_packet_rescale_ts(pkt: *mut AVPacket, tb_src: AVRational, tb_dst: AVRational);
}

// ---------------------------------------------------------------------------
// libavformat
// ---------------------------------------------------------------------------

extern "C" {
    pub fn avformat_alloc_output_context2(
        ctx: *mut *mut AVFormatContext,
        oformat: *const AVOutputFormat,
        format_name: *const c_char,
        filename: *const c_char,
    ) -> c_int;
    pub fn avformat_new_stream(oc: *mut AVFormatContext, c: *const AVCodec) -> *mut AVStream;
    pub fn avformat_write_header(oc: *mut AVFormatContext, options: *mut *mut c_void) -> c_int;
    pub fn av_interleaved_write_frame(oc: *mut AVFormatContext, pkt: *mut AVPacket) -> c_int;
    pub fn av_write_trailer(oc: *mut AVFormatContext) -> c_int;
    pub fn avformat_free_context(oc: *mut AVFormatContext);

    pub fn avio_open(s: *mut *mut AVIOContext, url: *const c_char, flags: c_int) -> c_int;
    pub fn avio_closep(s: *mut *mut AVIOContext) -> c_int;

    pub fn avformat_open_input(
        ps: *mut *mut AVFormatContext,
        url: *const c_char,
        fmt: *const c_void,
        options: *mut *mut c_void,
    ) -> c_int;
    pub fn avformat_find_stream_info(ic: *mut AVFormatContext, options: *mut *mut c_void) -> c_int;
    pub fn av_find_best_stream(
        ic: *mut AVFormatContext,
        type_: c_int,
        wanted_stream_nb: c_int,
        related_stream: c_int,
        decoder_ret: *mut *const AVCodec,
        flags: c_int,
    ) -> c_int;
    pub fn av_read_frame(s: *mut AVFormatContext, pkt: *mut AVPacket) -> c_int;
    pub fn avformat_close_input(s: *mut *mut AVFormatContext);
}

// ---------------------------------------------------------------------------
// libswscale
// ---------------------------------------------------------------------------

extern "C" {
    pub fn sws_getContext(
        src_w: c_int,
        src_h: c_int,
        src_format: c_int,
        dst_w: c_int,
        dst_h: c_int,
        dst_format: c_int,
        flags: c_int,
        src_filter: *mut c_void,
        dst_filter: *mut c_void,
        param: *const f64,
    ) -> *mut SwsContext;

    pub fn sws_scale(
        c: *mut SwsContext,
        src_slice: *const *const u8,
        src_stride: *const c_int,
        src_slice_y: c_int,
        src_slice_h: c_int,
        dst: *const *mut u8,
        dst_stride: *const c_int,
    ) -> c_int;

    pub fn sws_freeContext(ctx: *mut SwsContext);
}
