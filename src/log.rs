//! 运行日志：GUI 是 windows 子系统，双击启动没有控制台，出问题只能靠落盘。
//!
//! 开关策略（默认**不打扰**用户）：
//! * **debug 构建默认开启**，写到 `%LOCALAPPDATA%\CanLapse\canlapse.log`
//!   —— 用户测试时直接跑 `target\debug\CanLapse.exe` 就够了，不需要设任何环境变量；
//! * release 构建默认关闭，需要时用 `CANLAPSE_LOG=<路径>` 显式开启；
//! * `CANLAPSE_LOG` 对两种构建都优先。
//!
//! 为什么不用 `env_logger` 之类：这个项目刻意零 UI/运行时依赖，一个 60 行的追加写入
//! 就够用，且日志点都在低频路径（拾取/开始/停止/合成），每次 open+append 的代价无所谓。
//!
//! 日志里**不写**任何用户画面内容，只写窗口句柄、类名、尺寸、错误串这类诊断信息。

use std::path::PathBuf;
use std::sync::OnceLock;

static LOG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// 日志文件上限：超过就轮转成 `.old`（只留一份），避免用户长期开着越写越大。
const MAX_BYTES: u64 = 4 * 1024 * 1024;

fn default_path() -> Option<PathBuf> {
    let base = std::env::var("LOCALAPPDATA").ok()?;
    Some(PathBuf::from(base).join("CanLapse").join("canlapse.log"))
}

/// 初始化日志（可重复调用；只有第一次生效）。返回实际日志路径（未开启时为 None）。
pub fn init() -> Option<PathBuf> {
    LOG_PATH
        .get_or_init(|| {
            let path = std::env::var("CANLAPSE_LOG")
                .ok()
                .map(PathBuf::from)
                .or_else(|| {
                    if cfg!(debug_assertions) {
                        default_path()
                    } else {
                        None
                    }
                })?;
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            // 轮转：本次启动前就超限的话，把旧文件挪走
            if let Ok(meta) = std::fs::metadata(&path) {
                if meta.len() > MAX_BYTES {
                    let _ = std::fs::rename(&path, path.with_extension("log.old"));
                }
            }
            Some(path)
        })
        .clone()
}

pub fn path() -> Option<PathBuf> {
    LOG_PATH.get().cloned().flatten()
}

pub fn enabled() -> bool {
    path().is_some()
}

/// 写一行日志（自动加毫秒级时间戳）。未初始化 / 未开启时是空操作。
pub fn line(msg: &str) {
    let Some(Some(p)) = LOG_PATH.get() else {
        return;
    };
    let st = crate::win::local_now();
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
        let _ = writeln!(
            f,
            "[{:02}:{:02}:{:02}.{:03}] {}",
            st.wHour, st.wMinute, st.wSecond, st.wMilliseconds, msg
        );
    }
}

/// `logline!` 与 `log::line(&format!(..))` 等价，只是少打一层括号
#[macro_export]
macro_rules! logline {
    ($($arg:tt)*) => {
        $crate::log::line(&format!($($arg)*))
    };
}
