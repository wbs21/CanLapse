//! 链接「自编译最小 ffmpeg（libopenh264）」并把运行时 DLL 部署到 exe 同目录。
//!
//! 为什么必须部署 DLL：我们用的是**导入库静态链接**，`avcodec-63.dll` 等会在
//! **进程启动时**由加载器解析 —— 那时 `main()` 还没执行，
//! 所以"在 main 里 SetDllDirectory"这种做法根本来不及。DLL 必须位于
//! exe 同目录（或系统搜索路径）里。
//!
//! 依赖位置可用环境变量覆盖：
//!   * `CANLAPSE_FFMPEG_MIN`（或 `FFMPEG_MIN_DIR`）—— 默认 `D:\CapLite\ffmpeg-min`
//!   * `CANLAPSE_WINPTHREAD`   —— 直接指定 `libwinpthread-1.dll` 的完整路径
//!
//! ⚠️ `libwinpthread-1.dll` 不能用 `C:\mingw64\bin` 那份：那是 UCRT 系，
//! 与这套 msvcrt 系的 ffmpeg 不匹配，会在启动时报 0xC0000139（实测）。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// 运行时需要的 DLL：4 个 av* 由我们的导入表直接引用，
/// swresample 与 avfilter 没有；libwinpthread 是 avcodec 的依赖。
const AV_DLL_PREFIXES: [&str; 5] = [
    "avformat-", "avcodec-", "avutil-", "swscale-", "swresample-",
];

fn main() {
    let root = env::var("CANLAPSE_FFMPEG_MIN")
        .or_else(|_| env::var("FFMPEG_MIN_DIR"))
        .unwrap_or_else(|_| r"D:\CapLite\ffmpeg-min".to_string());
    let root = PathBuf::from(root);
    let bin = root.join("bin");

    let avcodec_lib = bin.join("avcodec.lib");
    if !avcodec_lib.exists() {
        panic!(
            "找不到 {}。\n\
             请设置环境变量 CANLAPSE_FFMPEG_MIN 指向自编译最小 ffmpeg 的根目录\n\
             （该目录下应有 bin\\avcodec.lib 等 MSVC 导入库，以及 bin\\*.dll）。",
            avcodec_lib.display()
        );
    }

    println!("cargo:rustc-link-search=native={}", bin.display());
    // MSVC 下顺序有意义：依赖方在前
    for lib in ["avformat", "avcodec", "swscale", "avutil"] {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
    println!("cargo:rerun-if-env-changed=CANLAPSE_FFMPEG_MIN");
    println!("cargo:rerun-if-env-changed=FFMPEG_MIN_DIR");
    println!("cargo:rerun-if-env-changed=CANLAPSE_WINPTHREAD");

    embed_manifest();

    deploy_runtime_dlls(&bin, &root);
}

/// 把 `CanLapse.manifest` 链接进 exe。
///
/// 它带来两件实在的事：
/// 1. **现代主题控件**（comctl32 v6）—— 没有它，按钮/滚动条/进度条是 Windows 95 外观；
/// 2. **PerMonitorV2 DPI 感知** —— 且能在进程启动前生效，避免首帧按错误缩放布局。
///
/// 只对 MSVC 目标有效（用的是 link.exe 的 `/MANIFESTINPUT`）。
fn embed_manifest() {
    let Ok(dir) = env::var("CARGO_MANIFEST_DIR") else {
        return;
    };
    let manifest = PathBuf::from(dir).join("CanLapse.manifest");
    if !manifest.is_file() {
        println!("cargo:warning=缺少 CanLapse.manifest，控件会退回经典外观");
        return;
    }
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_env != "msvc" {
        println!("cargo:warning=非 MSVC 目标，跳过清单嵌入");
        return;
    }
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg=/MANIFESTINPUT:{}",
        manifest.display()
    );
    println!("cargo:rerun-if-changed=CanLapse.manifest");
}

/// 把运行时 DLL 复制到「exe 同目录」和「exe 同目录\deps」。
/// 后者是为了 `cargo test`：测试可执行文件在 `deps\` 下，加载器只会在
/// 该 exe 自己的目录里找，不会去看上一级。
fn deploy_runtime_dlls(bin: &Path, root: &Path) {
    let out_dir = match env::var("OUT_DIR") {
        Ok(v) => PathBuf::from(v),
        Err(_) => return,
    };
    // OUT_DIR = <target>/<profile>/build/<pkg>-<hash>/out
    let Some(profile_dir) = out_dir.ancestors().nth(3).map(Path::to_path_buf) else {
        println!("cargo:warning=无法从 OUT_DIR 推断 exe 目录，跳过 DLL 部署");
        return;
    };
    let deps_dir = profile_dir.join("deps");
    let mut targets = vec![profile_dir.clone()];
    if deps_dir.is_dir() {
        targets.push(deps_dir);
    }

    let mut wanted: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = fs::read_dir(bin) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.to_ascii_lowercase().ends_with(".dll")
                && AV_DLL_PREFIXES
                    .iter()
                    .any(|p| name.to_ascii_lowercase().starts_with(p))
            {
                wanted.push(e.path());
            }
        }
    }
    if wanted.is_empty() {
        println!(
            "cargo:warning={} 下没找到 av*.dll，运行时会启动失败",
            bin.display()
        );
    }

    if let Some(p) = find_winpthread(root) {
        wanted.push(p);
    } else {
        println!(
            "cargo:warning=没找到 libwinpthread-1.dll（avcodec-63.dll 的依赖）。\
             可用环境变量 CANLAPSE_WINPTHREAD 指定；否则程序启动会报 0xC0000135。"
        );
    }

    for dir in &targets {
        for src in &wanted {
            let Some(name) = src.file_name() else { continue };
            let dst = dir.join(name);
            let same = match (fs::metadata(src), fs::metadata(&dst)) {
                (Ok(a), Ok(b)) => a.len() == b.len(),
                _ => false,
            };
            if !same {
                match fs::copy(src, &dst) {
                    Ok(_) => println!("cargo:warning=已部署 {}", dst.display()),
                    Err(e) => println!(
                        "cargo:warning=复制 {} -> {} 失败: {e}",
                        src.display(),
                        dst.display()
                    ),
                }
            }
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
}

/// 找一个可用的 `libwinpthread-1.dll`。
/// **故意不包含 `C:\mingw64\bin`**：那份是 UCRT 系，加载即 0xC0000139。
fn find_winpthread(root: &Path) -> Option<PathBuf> {
    if let Ok(p) = env::var("CANLAPSE_WINPTHREAD") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
        println!("cargo:warning=CANLAPSE_WINPTHREAD={} 不是文件，忽略", p.display());
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(parent) = root.parent() {
        candidates.push(parent.join("build").join("libwinpthread-1.dll"));
        candidates.push(
            parent
                .join("tools")
                .join("msys64")
                .join("mingw64")
                .join("bin")
                .join("libwinpthread-1.dll"),
        );
    }
    candidates.push(PathBuf::from(
        r"D:\tools\msys64\mingw64\bin\libwinpthread-1.dll",
    ));
    candidates.into_iter().find(|p| p.is_file())
}
