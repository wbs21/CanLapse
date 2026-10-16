//! 视频合成：把会话缓存里的 `frame_xxxx.png` 批量编码成 MP4。
//!
//! 目标实现（需求）：**自编译最小 OpenH264 方案，只保留编码、去除解码**，
//! 导出参数（播放帧率 / 画质 / 输出路径）独立于录制参数。
//!
//! ⚠️ 当前状态：**尚未接入**。接入方式正在 `spikes/encode-mp4/` 里验证
//! （离线环境、MSVC target、`D:\CapLite\ffmpeg-min` 与独立 `openh264.dll` 两条路线）。
//! 在验证结论落地前，本模块只提供接口与占位实现，**不会静默产出坏文件**：
//! 调用它一定会返回 `Err`，而不是生成一个假的 MP4。

use std::path::{Path, PathBuf};

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
}

#[derive(Clone, Debug)]
pub struct EncodeOptions {
    /// 导出播放帧率（5/10/15/20/24）——与录制帧密度无关，这是"缩时"的核心
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

/// 把会话目录里的 PNG 帧合成 MP4。
///
/// `progress(已处理帧数, 总帧数)` 会被定期回调，供 UI 显示进度并支持取消
/// （取消语义：回调里返回取消信号后由调用方处理，本函数暂不实现取消）。
pub fn encode_session(
    session_dir: &Path,
    opts: &EncodeOptions,
    progress: &mut dyn FnMut(u32, u32),
) -> Result<EncodeStats, String> {
    let frames = super::session::Session::list(session_dir.parent().unwrap_or(session_dir))
        .into_iter()
        .find(|s| s.dir == session_dir)
        .map(|s| s.frames)
        .unwrap_or(0);
    progress(0, frames);
    Err(format!(
        "视频合成尚未接入：正在验证自编译最小 OpenH264 的接入方式（见 spikes/encode-mp4/）。\
         会话目录 {} 已保留，接入后可直接重新导出（{} 帧，目标 {} fps，画质 {:?}）。",
        session_dir.display(),
        frames,
        opts.fps,
        opts.quality
    ))
}
