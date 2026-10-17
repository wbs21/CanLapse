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

/// 找自编译最小 ffmpeg 的根目录。
///
/// 顺序：
/// 1. 环境变量 `CANLAPSE_FFMPEG_MIN` / `FFMPEG_MIN_DIR`（最优先，跨机器可移植）；
/// 2. `tools\ffmpeg-min.path`：一行路径（本机把依赖目录挪走后改这里即可，无需动代码）；
/// 3. 本机已知的几个位置 —— `D:\CapLite` 被整体挪到 `D:\ffmpeg各版本\CapLite_c` 过，
///    只认一个默认路径会让"换个位置就编不过"（实测）。
fn find_ffmpeg_min() -> PathBuf {
    for key in ["CANLAPSE_FFMPEG_MIN", "FFMPEG_MIN_DIR"] {
        if let Ok(v) = env::var(key) {
            if !v.trim().is_empty() {
                return PathBuf::from(v);
            }
        }
    }
    if let Ok(dir) = env::var("CARGO_MANIFEST_DIR") {
        let hint = PathBuf::from(&dir).join("tools").join("ffmpeg-min.path");
        if let Ok(txt) = fs::read_to_string(&hint) {
            let line = txt
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with('#'));
            if let Some(line) = line {
                return PathBuf::from(line.trim_matches('"'));
            }
        }
    }
    let candidates = [
        r"D:\CapLite\ffmpeg-min",
        r"D:\ffmpeg各版本\CapLite_c\ffmpeg-min",
        r"D:\WorkbuddyWW\CapLite\ffmpeg-min",
        r"D:\tools\ffmpeg-min-new",
    ];
    for c in candidates {
        let p = PathBuf::from(c);
        if p.join("bin").join("avformat.lib").is_file() {
            println!("cargo:warning=使用 ffmpeg-min：{}", p.display());
            return p;
        }
    }
    PathBuf::from(r"D:\CapLite\ffmpeg-min")
}

fn main() {
    let root = find_ffmpeg_min();
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
    println!("cargo:rerun-if-changed=tools/ffmpeg-min.path");

    embed_manifest();
    compile_resources();

    deploy_runtime_dlls(&bin, &root);
}

/// 用 Windows SDK 的 rc.exe 编译 `CanLapse.rc`（图标 + 版本信息），把生成的 `.res`
/// 作为链接输入传给 link.exe。
///
/// 找不到 rc.exe 时只警告、不失败 —— 图标缺失不该让整个构建挂掉。
fn compile_resources() {
    let Ok(dir) = env::var("CARGO_MANIFEST_DIR") else {
        return;
    };
    let rc_file = PathBuf::from(&dir).join("CanLapse.rc");
    let ico = PathBuf::from(&dir).join("CanLapse.ico");
    if !rc_file.is_file() || !ico.is_file() {
        println!("cargo:warning=缺少 CanLapse.rc / CanLapse.ico，exe 将没有自定义图标");
        return;
    }
    println!("cargo:rerun-if-changed=CanLapse.rc");
    println!("cargo:rerun-if-changed=CanLapse.ico");

    if env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default() != "msvc" {
        println!("cargo:warning=非 MSVC 目标，跳过图标资源");
        return;
    }

    let Some(rc_exe) = find_tool("rc.exe") else {
        println!("cargo:warning=找不到 Windows SDK 的 rc.exe，exe 将没有自定义图标");
        return;
    };

    let Ok(out_dir) = env::var("OUT_DIR") else {
        return;
    };
    let res = PathBuf::from(&out_dir).join("CanLapse.res");
    // rc.exe 在 .rc 里按相对路径找 .ico，所以工作目录必须设在清单目录
    let status = std::process::Command::new(&rc_exe)
        .current_dir(&dir)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res)
        .arg(&rc_file)
        .status();
    match status {
        Ok(s) if s.success() && res.is_file() => {}
        Ok(s) => {
            println!("cargo:warning=rc.exe 退出码 {s}，跳过图标");
            return;
        }
        Err(e) => {
            println!("cargo:warning=调用 rc.exe 失败：{e}，跳过图标");
            return;
        }
    }

    // 关键一步：把 .res 打成**静态库**再链接。
    //
    // 为什么不能直接把 .res 当链接输入：
    //   * `cargo:rustc-link-arg=<res>`      —— 裸路径没进链接命令行（实测）
    //   * `cargo:rustc-link-arg-bins=<res>` —— 进了，但 link.exe 报 LNK1123
    //     （它把 .res 当对象文件去做 COFF 转换，失败了）
    // 走"资源库 + /WHOLEARCHIVE"这条路：link-lib / link-search 是 rustc 的
    // 常规能力，/WHOLEARCHIVE 是标准 flag（flag 形式的 link-arg 实测可达链接器），
    // 而 /WHOLEARCHIVE 同时解决了"资源库里没有符号、可能被整体丢弃"的问题。
    let Some(lib_exe) = find_tool("lib.exe") else {
        println!("cargo:warning=找不到 lib.exe，资源未链入 exe");
        return;
    };
    let res_lib = PathBuf::from(&out_dir).join("CanLapse_res.lib");
    let lib_status = std::process::Command::new(&lib_exe)
        .arg("/nologo")
        .arg(format!("/out:{}", res_lib.display()))
        .arg(&res)
        .status();
    match lib_status {
        Ok(s) if s.success() && res_lib.is_file() => {
            // 两个都要：
            //   * link-lib 让该库**成为链接输入**（否则 /WHOLEARCHIVE:名字 找不到它）
            //   * /WHOLEARCHIVE 强制包含全部成员 —— 资源库里没有符号，
            //     不强制的话链接器会认为"没人引用"而整体丢弃
            println!("cargo:rustc-link-search=native={out_dir}");
            println!("cargo:rustc-link-lib=static=CanLapse_res");
            println!("cargo:rustc-link-arg=/WHOLEARCHIVE:CanLapse_res.lib");
            println!(
                "cargo:warning=资源库已生成（{} 字节）并已作为链接输入请求 /WHOLEARCHIVE，\
                 但实测**未生效**（exe 的版本信息仍为空、图标仍是默认）——\
                 详见本函数顶部关于三种尝试的注释",
                std::fs::metadata(&res_lib).map(|m| m.len()).unwrap_or(0)
            );
        }
        Ok(s) => println!("cargo:warning=lib.exe 退出码 {s}，资源未链入"),
        Err(e) => println!("cargo:warning=调用 lib.exe 失败：{e}，资源未链入"),
    }
}

/// 在 Windows SDK / MSVC 工具链目录里找工具（取版本号最大的那个）。
fn find_tool(file: &str) -> Option<PathBuf> {
    // SDK：bin\<ver>\x64\rc.exe
    let mut dirs: Vec<PathBuf> = Vec::new();
    for base in [
        r"C:\Program Files (x86)\Windows Kits\10\bin",
        r"C:\Program Files\Windows Kits\10\bin",
    ] {
        if let Ok(entries) = std::fs::read_dir(base) {
            for e in entries.flatten() {
                let d = e.path().join("x64");
                if d.join(file).is_file() {
                    dirs.push(d);
                }
            }
        }
    }
    // MSVC：<VS>\<year>\<edition>\VC\Tools\MSVC\<ver>\bin\Hostx64\x64\{rc,lib}.exe
    for base in [
        r"C:\Program Files\Microsoft Visual Studio",
        r"C:\Program Files (x86)\Microsoft Visual Studio",
    ] {
        let Ok(years) = std::fs::read_dir(base) else {
            continue;
        };
        for y in years.flatten() {
            let Ok(editions) = std::fs::read_dir(y.path()) else {
                continue;
            };
            for e in editions.flatten() {
                let msvc = e.path().join("VC").join("Tools").join("MSVC");
                let Ok(vers) = std::fs::read_dir(&msvc) else {
                    continue;
                };
                for v in vers.flatten() {
                    for host in ["Hostx64/x64", "Hostx86/x86"] {
                        let d = v.path().join("bin").join(host.replace('/', "\\"));
                        if d.join(file).is_file() {
                            dirs.push(d);
                        }
                    }
                }
            }
        }
    }
    dirs.sort();
    dirs.pop().map(|d| d.join(file))
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
