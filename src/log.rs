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

use std::path::PathBuf;
use std::sync::OnceLock;

/// 实际启用的日志文件（第一个是主日志，第二个通常是 exe 目录的镜像）。
static SINKS: OnceLock<Vec<PathBuf>> = OnceLock::new();

/// 单个日志文件上限：超过就轮转成 `.old`（只留一份），避免长期开着越写越大。
const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// 尝试打开（必要时创建目录和文件）；成功即认为这个位置可写。
fn writable(p: &PathBuf) -> bool {
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)
        .is_ok()
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
            let mut sinks: Vec<PathBuf> = Vec::new();
            // 主日志 = 第一个可写的位置
            for p in &cands {
                if writable(p) {
                    rotate(p);
                    sinks.push(p.clone());
                    break;
                }
            }
            // 镜像 = exe 同目录（与主日志不同、且可写）；写不进去就算了
            if let Ok(exe) = std::env::current_exe() {
                if let Some(dir) = exe.parent() {
                    let mirror = dir.join("canlapse.log");
                    if !sinks.contains(&mirror) && writable(&mirror) {
                        rotate(&mirror);
                        sinks.push(mirror);
                    }
                }
            }
            sinks
        })
        .first()
        .cloned()
}

/// 当前启用的全部日志路径（主日志在前）。
pub fn sinks() -> Vec<PathBuf> {
    SINKS.get().cloned().unwrap_or_default()
}

pub fn path() -> Option<PathBuf> {
    sinks().into_iter().next()
}

pub fn enabled() -> bool {
    !sinks().is_empty()
}

/// 写一行日志（自动加毫秒级时间戳）。未初始化 / 未开启时是空操作。
pub fn line(msg: &str) {
    let Some(sinks) = SINKS.get() else {
        return;
    };
    if sinks.is_empty() {
        return;
    }
    let st = crate::win::local_now();
    use std::io::Write;
    let text = format!(
        "[{:02}:{:02}:{:02}.{:03}] {}\n",
        st.wHour, st.wMinute, st.wSecond, st.wMilliseconds, msg
    );
    for p in sinks {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = f.write_all(text.as_bytes());
        }
    }
}

/// `logline!` 与 `log::line(&format!(..))` 等价，只是少打一层括号
#[macro_export]
macro_rules! logline {
    ($($arg:tt)*) => {
        $crate::log::line(&format!($($arg)*))
    };
}
