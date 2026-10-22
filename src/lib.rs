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
pub mod fsutil;
pub mod img_io;
pub mod lang;
pub mod log;
pub mod navigator;
pub mod recorder;
pub mod session;
pub mod ui;
pub mod win;

/// 单帧/单张缓存图片允许的最大像素数（约 8K：7680×4320 = 33.2MP）。
/// 这是防止异常窗口或损坏缓存导致数 GB 分配的硬上限；正常 4K 画布远低于它。
pub const MAX_CAPTURE_PIXELS: u64 = 40_000_000;
/// 单边尺寸上限，防止极端长条窗口产生病态布局/尺寸计算。
pub const MAX_CAPTURE_DIM: i32 = 16_384;
/// 单张缓存图片文件大小上限（防止读取异常大的本地文件）。
pub const MAX_IMAGE_FILE_BYTES: u64 = 128 * 1024 * 1024;

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
    std::path::PathBuf::new()
}

// ---------------------------------------------------------------------------
// 目录策略（v1.0.0 收工后增补）
// ---------------------------------------------------------------------------
//
// 用户要求"缓存和成片都放在程序所在文件夹"，便于绿色分发：解压到哪，数据就在哪。
// 但程序可能被装在 Program Files 这类不可写目录，所以统一走"先程序目录、写不了再退
// LOCALAPPDATA"，并且**环境变量 CANLAPSE_CACHE 仍然优先**（脚本/测试要用）。

fn exe_dir() -> Option<std::path::PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

fn dir_writable(dir: &std::path::Path) -> bool {
    match crate::fsutil::create_temp(&dir.join("canlapse-write-test")) {
        Ok((probe, file)) => {
            drop(file);
            let _ = std::fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
}

/// 在"程序目录下的子目录"和"%LOCALAPPDATA%\CanLapse\<子目录>"之间选一个可写的位置
fn preferred_dir(sub: &str) -> std::path::PathBuf {
    if let Some(dir) = exe_dir() {
        let p = dir.join(sub);
        if std::fs::create_dir_all(&p).is_ok() && dir_writable(&p) {
            return p;
        }
    }
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    let p = std::path::PathBuf::from(base).join("CanLapse").join(sub);
    let _ = std::fs::create_dir_all(&p);
    p
}

/// 会话缓存根目录：`CANLAPSE_CACHE` > `<exe 目录>\cache` > `%LOCALAPPDATA%\CanLapse\cache`
pub fn default_cache_root() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("CANLAPSE_CACHE") {
        if !p.trim().is_empty() {
            return std::path::PathBuf::from(p);
        }
    }
    preferred_dir("cache")
}

/// 默认导出目录：`<exe 目录>\output` > `%LOCALAPPDATA%\CanLapse\output`
pub fn default_output_dir() -> std::path::PathBuf {
    preferred_dir("output")
}
