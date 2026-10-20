//! 运行日志：GUI 是 windows 子系统，双击启动没有控制台，出问题只能靠落盘。
//!
//! 落盘策略（**默认开启**，`CANLAPSE_NOLOG=1` 可关）：
//! * 主日志：`%LOCALAPPDATA%\CanLapse\canlapse.log`；
//! * **镜像**：exe 同目录的 `canlapse.log` —— 万一本机/沙箱不让写 `%LOCALAPPDATA%`，
//!   日志仍然落在程序旁边（用户测试后直接把 exe 目录里的那份发出来即可）；
//! * `CANLAPSE_LOG=<路径>` 可显式指定主日志位置（优先级最高）。
//!
//! 为什么不再按 debug/release 区分：用户拿到的往往是 release 版，
//! "只有 debug 才有日志" 等于下次还是没数据（实测踩过：用户跑了 release，日志目录根本没生成）。
//! 这个工具一天也写不了几 KB，一直开着没有实际代价。
//!
//! 为什么不用 `env_logger` 之类：项目刻意零 UI/运行时依赖，一个追加写入就够用；
//! 日志点都在低频路径（拾取/开始/停止/合成），每次 open+append 的代价无所谓。
//!
//! 日志里**不写**任何用户画面内容，只写窗口句柄、类名、尺寸、错误串这类诊断信息。

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Cursor, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// 实际启用的日志文件（第一个是主日志，第二个通常是 exe 目录的镜像）。
/// 存为 `(PathBuf, Mutex<BufWriter<File>>)` —— 句柄持久打开，避免每次 `line()` 都 open+close。
static SINKS: OnceLock<Vec<(PathBuf, Mutex<BufWriter<File>>)>> = OnceLock::new();
/// 预打开的崩溃日志句柄。未处理异常回调里不能可靠地做路径解析/分配，
/// 因此启动时打开好文件，异常时只做一次固定缓冲写入。
static CRASH: OnceLock<File> = OnceLock::new();

/// 单个日志文件上限：超过就轮转成 `.old`（只留一份），避免长期开着越写越大。
const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// 尝试打开（必要时创建目录和文件）；成功返回打开的文件。
fn open_log(p: &PathBuf) -> Option<File> {
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    OpenOptions::new().create(true).append(true).open(p).ok()
}

fn rotate(p: &PathBuf) {
    if let Ok(meta) = std::fs::metadata(p) {
        if meta.len() > MAX_BYTES {
            let _ = std::fs::rename(p, p.with_extension("log.old"));
        }
    }
}

/// 候选位置：显式指定 > LOCALAPPDATA > exe 同目录 > TEMP。
fn candidates() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("CANLAPSE_LOG") {
        if !p.trim().is_empty() {
            v.push(PathBuf::from(p));
        }
    }
    if let Some(base) = std::env::var_os("LOCALAPPDATA") {
        v.push(PathBuf::from(base).join("CanLapse").join("canlapse.log"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join("canlapse.log"));
        }
    }
    if let Some(tmp) = std::env::var_os("TEMP") {
        v.push(PathBuf::from(tmp).join("CanLapse").join("canlapse.log"));
    }
    v
}

/// 初始化日志（可重复调用；只有第一次生效）。返回主日志路径（全部不可写时 None）。
pub fn init() -> Option<PathBuf> {
    SINKS
        .get_or_init(|| {
            if std::env::var("CANLAPSE_NOLOG").is_ok() {
                return Vec::new();
            }
            let cands = candidates();
            let mut sinks: Vec<(PathBuf, Mutex<BufWriter<File>>)> = Vec::new();
            // 主日志 = 第一个可写的位置
            for p in &cands {
                rotate(p);
                if let Some(f) = open_log(p) {
                    sinks.push((p.clone(), Mutex::new(BufWriter::new(f))));
                    break;
                }
            }
            // 镜像 = exe 同目录（与主日志不同、且可写）；写不进去就算了
            if let Ok(exe) = std::env::current_exe() {
                if let Some(dir) = exe.parent() {
                    let mirror = dir.join("canlapse.log");
                    if !sinks.iter().any(|(path, _)| path == &mirror) {
                        rotate(&mirror);
                        if let Some(f) = open_log(&mirror) {
                            sinks.push((mirror, Mutex::new(BufWriter::new(f))));
                        }
                    }
                }
            }
            // 预打开 crash.log
            if let Some((primary, _)) = sinks.first() {
                if let Some(dir) = primary.parent() {
                    if let Some(f) = open_log(&dir.join("crash.log")) {
                        let _ = CRASH.set(f);
                    }
                }
            }
            sinks
        })
        .first()
        .map(|(p, _)| p.clone())
}

/// 当前启用的全部日志路径（主日志在前）。
pub fn sinks() -> Vec<PathBuf> {
    SINKS
        .get()
        .map(|s| s.iter().map(|(p, _)| p.clone()).collect())
        .unwrap_or_default()
}

pub fn path() -> Option<PathBuf> {
    sinks().into_iter().next()
}

pub fn enabled() -> bool {
    !sinks().is_empty()
}

/// 写一行日志（自动加毫秒级时间戳）。未初始化 / 未开启时是空操作。
/// 句柄是持久打开的 BufWriter —— 这里只做一次 Mutex 加锁 + 一次 BufWriter 写入。
pub fn line(msg: &str) {
    let Some(sinks) = SINKS.get() else {
        return;
    };
    if sinks.is_empty() {
        return;
    }
    let st = crate::win::local_now();
    let text = format!(
        "[{:02}:{:02}:{:02}.{:03}] {}\n",
        st.wHour, st.wMinute, st.wSecond, st.wMilliseconds, msg
    );
    for (path, writer) in sinks {
        let mut w = match writer.lock() {
            Ok(g) => g,
            Err(_) => continue,
        };
        // 轮转检查：如果文件已超过 MAX_BYTES，先 flush 再 rename 并重新打开
        let _ = w.flush();
        let needs_rotate = std::fs::metadata(path)
            .map(|m| m.len() > MAX_BYTES)
            .unwrap_or(false);
        if needs_rotate {
            let _ = std::fs::rename(path, path.with_extension("log.old"));
            if let Some(new_file) = open_log(path) {
                *w = BufWriter::new(new_file);
            }
        }
        let _ = w.write_all(text.as_bytes());
    }
}

/// 未处理异常专用：固定栈缓冲 + 预打开句柄，避免在堆可能损坏时分配。
pub fn crash_line(code: u32, addr: usize) {
    let Some(file) = CRASH.get() else {
        return;
    };
    let mut buf = [0u8; 192];
    let mut cursor = Cursor::new(&mut buf[..]);
    let _ = writeln!(cursor, "[fatal] code=0x{code:08X} addr=0x{addr:X}");
    let n = cursor.position() as usize;
    let mut f = file;
    let _ = f.write_all(&buf[..n]);
    let _ = f.flush();
}

/// `logline!` 与 `log::line(&format!(..))` 等价，只是少打一层括号
#[macro_export]
macro_rules! logline {
    ($($arg:tt)*) => {
        $crate::log::line(&format!($($arg)*))
    };
}
