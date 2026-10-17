//! CanLapse 库：所有功能模块都在这里，供两个入口共用。
//!
//! 两个可执行入口：
//! * `src/main.rs`（控制台子系统）—— 命令行：录制 / 合成 / 诊断
//! * `src/bin/canlapse-gui.rs`（windows 子系统）—— 图形界面，双击不弹控制台黑框
//!
//! 做成 lib 而不是让两个 bin 各自 `#[path]` 引入模块，有两个实在的理由：
//! 1. `#[path = "../encode.rs"] mod encode;` 会破坏子模块解析 ——
//!    `encode.rs` 里的 `mod ffi;` 会被解析到 `src/ffi.rs` 而不是 `src/encode/ffi.rs`；
//! 2. 同一份代码被两个 bin 各编译一遍，既慢又容易只改了一处。

pub mod capture;
pub mod encode;
pub mod frame;
pub mod log;
pub mod img_io;
pub mod recorder;
pub mod session;
pub mod ui;
pub mod win;

/// 解析自编译最小 ffmpeg 的根目录（doctor 用）。
///
/// 顺序与 `build.rs` 的 `find_ffmpeg_min` **保持一致**：
/// 环境变量 → `tools\ffmpeg-min.path`（一行路径）→ 本机已知位置。
/// 依赖目录被挪走过（`D:\CapLite` → `D:\ffmpeg各版本\CapLite_c`），
/// 只认一个默认路径会出现"cargo build 能过、doctor 指错目录"的错位。
pub fn ffmpeg_min_dir() -> std::path::PathBuf {
    for key in ["CANLAPSE_FFMPEG_MIN", "FFMPEG_MIN_DIR"] {
        if let Ok(v) = std::env::var(key) {
            if !v.trim().is_empty() {
                return std::path::PathBuf::from(v);
            }
        }
    }
    let hint = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tools")
        .join("ffmpeg-min.path");
    if let Ok(txt) = std::fs::read_to_string(&hint) {
        if let Some(line) = txt
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
        {
            return std::path::PathBuf::from(line.trim_matches('"'));
        }
    }
    for c in [
        r"D:\CapLite\ffmpeg-min",
        r"D:\ffmpeg各版本\CapLite_c\ffmpeg-min",
        r"D:\WorkbuddyWW\CapLite\ffmpeg-min",
        r"D:\tools\ffmpeg-min-new",
    ] {
        let p = std::path::PathBuf::from(c);
        if p.join("bin").join("avformat.lib").is_file() {
            return p;
        }
    }
    std::path::PathBuf::from(r"D:\CapLite\ffmpeg-min")
}
