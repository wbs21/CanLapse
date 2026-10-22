//! CanLapse 命令行切片。
//!
//! 当前流程：拾取窗口 → 按帧密度与差异阈值抓帧 → 流式 H.264 编码 →
//! 停止后无重编码重封装为普通 MP4。
//!
//! 用法见 `canlapse --help`。

// 模块都在 lib（src/lib.rs）里；这里按名字引入，CLI 用到的路径写法保持不变，
// 与 GUI 入口（src/bin/canlapse-gui.rs）共用同一份实现。
use canlapse::{capture, encode, navigator, recorder, session, ui, win};

use std::path::PathBuf;
use std::time::{Duration, Instant};

use session::Session;

// 目录策略统一在 lib.rs：CANLAPSE_CACHE > <exe 目录>\cache > %LOCALAPPDATA%\CanLapse\cache
fn cache_root() -> PathBuf {
    canlapse::default_cache_root()
}

struct RecordArgs {
    hwnd: Option<isize>,
    title: Option<String>,
    pick: bool,
    out: Option<PathBuf>,
    name: Option<String>,
    fps: f64,
    threshold: f32,
    duration: Option<f64>,
    quiet: bool,
    /// 关闭"其他软件在前台就暂停抓帧"（录后台窗口时必需）
    no_fg_pause: bool,
    /// 从用户指定的第二视图采集完整画布。
    full_view: bool,
    /// 旧兼容入口：自动查找 SAI2/CSP 导航器。
    navigator: bool,
}

impl Default for RecordArgs {
    fn default() -> Self {
        Self {
            hwnd: None,
            title: None,
            pick: false,
            out: None,
            name: None,
            fps: 2.0,        // 需求：帧密度 1~8 fps
            threshold: 0.01, // 需求：像素差异阈值 0.00~0.05
            duration: None,
            quiet: false,
            no_fg_pause: false,
            full_view: false,
            navigator: false,
        }
    }
}

fn parse_hex_hwnd(s: &str) -> Option<isize> {
    let t = s.trim_start_matches("0x").trim_start_matches("0X");
    isize::from_str_radix(t, 16).ok()
}

fn next_value(args: &[String], i: &mut usize) -> Option<String> {
    *i += 1;
    args.get(*i).cloned()
}

fn required_value(args: &[String], i: &mut usize, option: &str) -> Result<String, String> {
    next_value(args, i).ok_or_else(|| format!("{option} 缺少参数"))
}

fn parse_record_args(args: &[String]) -> Result<RecordArgs, String> {
    let mut a = RecordArgs::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--hwnd" => {
                let value = required_value(args, &mut i, "--hwnd")?;
                a.hwnd = Some(
                    parse_hex_hwnd(&value)
                        .ok_or_else(|| "--hwnd 需要十六进制句柄，例如 0x00120AB4".to_string())?,
                );
            }
            "--title" => a.title = Some(required_value(args, &mut i, "--title")?),
            "--pick" => a.pick = true,
            "--out" => a.out = Some(PathBuf::from(required_value(args, &mut i, "--out")?)),
            "--name" => a.name = Some(required_value(args, &mut i, "--name")?),
            "--fps" => {
                let value = required_value(args, &mut i, "--fps")?;
                a.fps = value
                    .parse()
                    .map_err(|_| format!("--fps 需要数字（收到 {value}）"))?;
            }
            "--threshold" => {
                let value = required_value(args, &mut i, "--threshold")?;
                a.threshold = value
                    .parse()
                    .map_err(|_| format!("--threshold 需要数字（收到 {value}）"))?;
            }
            "--duration" => {
                let value = required_value(args, &mut i, "--duration")?;
                let seconds: f64 = value
                    .parse()
                    .map_err(|_| format!("--duration 需要秒数（收到 {value}）"))?;
                if !seconds.is_finite() || seconds <= 0.0 {
                    return Err("--duration 必须是大于 0 的有限秒数".into());
                }
                a.duration = Some(seconds);
            }
            "--quiet" => a.quiet = true,
            "--no-fg-pause" => a.no_fg_pause = true,
            "--full-view" | "--full" => a.full_view = true,
            "--navigator" | "--nav" => a.navigator = true,
            other => return Err(format!("record 不支持参数 {other}")),
        }
        i += 1;
    }
    Ok(a)
}

fn help() {
    println!(
        r#"CanLapse —— 画布窗口缩时录屏工具（{version}）

用法:
  CanLapse                                         启动图形界面（双击这个 exe）
  canlapse-cli gui [--hwnd 0x… | --title 子串]     启动图形界面并预先指定目标窗口
  canlapse-cli list                                列出可见顶层窗口（挑目标用）
  canlapse-cli pick                                鼠标拾取画布窗口并打印结果
  canlapse-cli record [目标] [选项]                 录制并自动整理为普通 MP4
  canlapse-cli sessions                            列出历史缓存会话
  canlapse-cli encode --session DIR [选项]         把会话整理为普通 MP4
  canlapse-cli doctor                              自检（抓帧/编码/运行时 DLL/缓存）
  canlapse-cli --help

录制目标（三选一，都不给则进入鼠标拾取）:
  --hwnd 0x1234        直接指定窗口句柄
  --title 子串         按标题/类名/进程名匹配第一个窗口
  --pick               鼠标拾取

录制选项:
  --out DIR            缓存根目录（默认 %LOCALAPPDATA%\CanLapse\cache，或环境变量 CANLAPSE_CACHE）
  --name NAME          会话名（默认取目标进程名），目录名形如 2026-02-14_13-20-51_NAME
  --fps N              帧密度 1~8（默认 2）
  --threshold F        像素差异阈值 0.00~0.05（默认 0.01）
  --duration SEC       录制时长上限，0 或省略表示手动停止（Esc 或关闭画布窗口）
  --quiet              少打印
  --no-fg-pause        关闭"其他软件在前台就暂停抓帧"。
                       默认开启该规则（切到别的软件=没在画画，自动暂停）；
                       但命令行录**后台窗口**时必须关掉，否则会一直暂停、一帧都不出。
  --navigator          从 SAI2/CSP 分离导航器录制完整画布。
                       自动查找导航器、裁掉标题/边距；目标可省略。
  --full-view          从用户选择的 CSP 第二视图录制完整画布（推荐）。
                       自动裁剪窗口边框/标题并持续直接抓取源窗口。

导出选项（canlapse encode）:
  --session DIR        会话缓存目录（必填）
  --fps N              导出播放帧率，默认 10（常用 5/10/15/20/24）
  --quality LEVEL      画质档位 high/medium/low（也接受 高/中/低），默认 medium
  --out FILE           输出 MP4 路径（默认 <会话目录>\timelapse.mp4）
  --frames N           只导出前 N 帧，用于预览；走与【取消合成】相同的路径

说明:
  * 抓帧走 DWM 缩略图（宿主窗口 + PrintWindow(PW_RENDERFULLCONTENT)）：
    目标画布被其他窗口盖住、甚至被最小化，都仍能录到画面。
  * 录制过程中按 Esc 停止，按 F8 打一个标记（写入 markers.json）。
"#,
        version = env!("CARGO_PKG_VERSION")
    );
}

fn cmd_list() -> i32 {
    let ws = win::list_windows();
    println!(
        "{:<12} {:<22} {:<26} {:>10}  {:<18}  标题",
        "HWND", "进程", "类名", "客户区", "窗口位置/尺寸"
    );
    for w in &ws {
        let (cw, ch) = w.client_size();
        let rw = w.rect.right - w.rect.left;
        let rh = w.rect.bottom - w.rect.top;
        println!(
            "0x{:<10X} {:<22} {:<26} {:>10}  {:<18}  {}",
            w.hwnd as isize,
            w.process,
            w.class_name,
            format!("{cw}x{ch}"),
            format!("{},{} {}x{}", w.rect.left, w.rect.top, rw, rh),
            w.title
        );
    }
    println!("\n共 {} 个可见顶层窗口", ws.len());
    0
}

fn cmd_pick() -> i32 {
    match win::pick_window_interactive(std::ptr::null_mut(), None) {
        Some(h) => {
            let info = win::describe(h);
            println!("已选中: {}", info.label());
            println!("  HWND      = 0x{:X}", h as isize);
            println!("  进程      = {}", info.process);
            println!("  类名      = {}", info.class_name);
            println!(
                "  客户区    = {}x{}",
                info.client_size().0,
                info.client_size().1
            );
            println!("  已最小化  = {}", info.minimized);
            0
        }
        None => 1,
    }
}

fn cmd_sessions(root: &std::path::Path) -> i32 {
    let list = Session::list(root);
    if list.is_empty() {
        println!("缓存根目录 {} 下还没有会话。", root.display());
        return 0;
    }
    println!("{:<28} {:>8}  状态   目录", "会话", "帧数");
    for s in &list {
        let status = if s.completed { "完成" } else { "未完成" };
        println!(
            "{:<28} {:>8}  {:<6} {}",
            s.name,
            s.frames,
            status,
            s.dir.display()
        );
    }
    let incomplete = list.iter().filter(|s| !s.completed).count();
    println!("\n共 {} 个会话，其中 {} 个未完成", list.len(), incomplete);
    if incomplete > 0 {
        println!("可用 `encode --session <目录>` 重新导出未完成会话。");
    }
    0
}

fn resolve_target(a: &RecordArgs) -> Option<win::WindowInfo> {
    if a.navigator && !a.pick && a.hwnd.is_none() && a.title.is_none() {
        if let Some(main) = navigator::find_navigator_main() {
            return Some(main);
        }
        eprintln!("未找到受支持的 SAI2/CSP 窗口。请先启动软件并分离导航器。");
        return None;
    }
    if a.pick {
        let hwnd = win::pick_window_interactive(std::ptr::null_mut(), None)?;
        return Some(win::describe(hwnd));
    }
    if let Some(h) = a.hwnd {
        let hwnd = h as windows_sys::Win32::Foundation::HWND;
        if win::is_window_alive(hwnd) {
            return Some(win::describe(hwnd));
        }
        eprintln!("HWND 0x{h:X} 无效或已关闭");
        return None;
    }
    if let Some(t) = &a.title {
        match win::find_window(t) {
            Some(w) => return Some(w),
            None => {
                eprintln!("按 \"{t}\" 没找到窗口；用 `canlapse list` 看看有哪些");
                return None;
            }
        }
    }
    let hwnd = win::pick_window_interactive(std::ptr::null_mut(), None)?;
    Some(win::describe(hwnd))
}

fn unique_output_path(dir: &std::path::Path, stem: &str) -> PathBuf {
    let mut path = dir.join(format!("{stem}.mp4"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem}_{n}.mp4"));
        n += 1;
    }
    path
}

fn unique_timelapse_output() -> PathBuf {
    let dir = canlapse::default_output_dir();
    let _ = std::fs::create_dir_all(&dir);
    let stamp = win::stamp_compact(&win::local_now());
    unique_output_path(&dir, &format!("timelapse_{stamp}"))
}

fn cmd_record(a: RecordArgs) -> i32 {
    if a.full_view && a.navigator {
        eprintln!("--full-view 和 --navigator 不能同时使用");
        return 2;
    }
    if !a.full_view && !a.navigator && !capture::composition_enabled() {
        eprintln!("桌面合成（DWM）未开启，无法使用缩略图抓帧。");
        return 2;
    }
    if !(0.1..=8.0).contains(&a.fps) {
        eprintln!("帧密度必须在 0.1~8 之间（收到 {}）", a.fps);
        return 2;
    }
    if !(0.0..=0.05).contains(&a.threshold) {
        eprintln!("像素差异阈值必须在 0.00~0.05 之间（收到 {}）", a.threshold);
        return 2;
    }

    let info = match resolve_target(&a) {
        Some(i) => i,
        None => return 1,
    };
    if a.full_view
        && (info.process.eq_ignore_ascii_case("sai2.exe")
            || info.process.eq_ignore_ascii_case("sai.exe"))
    {
        eprintln!("SAI2 第二视图不稳定，请改用 --navigator 导航器模式");
        return 2;
    }
    println!("目标: {}", info.label());
    if a.navigator {
        match navigator::probe(info.hwnd) {
            Ok(nav) => {
                let (w, h) = nav.capture.client_size();
                println!(
                    "导航器: root=0x{:X} capture=0x{:X} {}x{}",
                    nav.root.hwnd as isize, nav.capture.hwnd as isize, w, h
                );
            }
            Err(e) => {
                eprintln!("导航器模式不可用: {e}");
                return 2;
            }
        }
    }

    let root = a.out.clone().unwrap_or_else(cache_root);
    let removed = canlapse::fsutil::cleanup_stale_temps(&root, canlapse::fsutil::STALE_TEMP_AGE);
    if removed > 0 {
        println!("已清理 {removed} 个过期临时缓存文件");
    }
    let name = a.name.clone().unwrap_or_else(|| {
        if info.process.is_empty() {
            "画布".into()
        } else {
            info.process.clone()
        }
    });
    println!(
        "录制参数: 帧密度 {}/s，差异阈值 {:.3}。按 Esc 停止{}。",
        a.fps,
        a.threshold,
        match a.duration {
            Some(d) => format!("，或 {d} 秒后自动停止"),
            None => String::new(),
        }
    );
    if a.full_view {
        println!("模式: 完整画布视图（第二视图，推荐）");
    } else if a.navigator {
        println!("模式: 导航器兼容模式");
    }
    if a.navigator {
        println!(
            "提示: 导航器模式会录制固定方向的全画布，并自动裁掉导航器标题和边距；\n\
             录制中请保持导航器尺寸不变。"
        );
    } else if a.full_view {
        println!(
            "提示: 请确保第二视图保持完整画布和固定方向；程序会自动裁掉标题栏和窗口边框，\n\
             并在窗口缩放时保持固定视频画面。"
        );
    } else {
        println!(
            "提示: 录制开始后**第一张保存的画面决定视频分辨率**。建议录制初期就把画布窗口调到目标大小；\n\
             录制中尽量不要缩小画布窗口（画布变小、后续帧会被拉伸放大，画面会糊；变大则会被缩小）。"
        );
    }

    // 抓帧/差分/分辨率统一都在录制线程里做（GDI 有线程亲和性，且不能阻塞 UI）。
    // 这里只负责：把按键转成线程指令 + 打印进度。
    let cfg = recorder::RecorderConfig {
        hwnd: info.hwnd as isize,
        root,
        name,
        fps: a.fps,
        threshold: a.threshold,
        fill_rgb: [0, 0, 0],
        fg_pause: !a.no_fg_pause,
        full_view: a.full_view,
        navigator: a.navigator,
    };
    let shared = recorder::Shared::new();
    let worker = recorder::spawn(cfg, shared.clone());
    let start = Instant::now();
    let mut seen_markers = 0u32;
    let mut stop_sent = false;

    loop {
        win::pump();
        if !stop_sent && win::escape_pressed_once() {
            println!("\n收到 Esc，结束录制。");
            shared.request_stop();
            stop_sent = true;
        }
        if win::marker_key_pressed() {
            shared.request_mark();
        }
        let phase = shared.phase();
        if phase == recorder::Phase::Finished || phase == recorder::Phase::Failed {
            break;
        }
        if shared.markers() != seen_markers {
            seen_markers = shared.markers();
            println!("\n已打标记 #{seen_markers}（第 {} 帧处）", shared.frames());
        }
        if !a.quiet && phase.is_running() {
            let (bw, bh) = shared.base_size();
            let base = if bw > 0 {
                format!("{bw}x{bh}")
            } else {
                "待定".to_string()
            };
            // 用户规则：其他软件在前台 => 此刻没在画画 => 自动暂停抓帧
            let warn = if shared.fg_paused() {
                " | ⏸ 其他软件在前台，已暂停抓帧（切回画布即继续）".to_string()
            } else {
                String::new()
            };
            print!(
                "\r已录制 {} 帧 | 基准 {base} | 丢弃 {} | {}{warn}   ",
                shared.frames(),
                shared.dropped(),
                recorder::format_duration(shared.elapsed_ms())
            );
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
        if !stop_sent {
            if let Some(d) = a.duration {
                if start.elapsed().as_secs_f64() >= d {
                    println!("\n达到指定时长 {d} 秒，结束录制。");
                    shared.request_stop();
                    stop_sent = true;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = worker.join();

    if let Some(e) = shared.error() {
        eprintln!("录制失败: {e}");
        return 2;
    }
    println!(
        "\n录制结束：{} 帧，{} 个标记，丢弃 {}，耗时 {}",
        shared.frames(),
        shared.markers(),
        shared.dropped(),
        recorder::format_duration(shared.elapsed_ms())
    );
    if let Some(dir) = shared.session_dir() {
        println!("会话目录: {}", dir.display());
        if shared.frames() > 0 {
            let output = unique_timelapse_output();
            let opts = encode::EncodeOptions {
                fps: encode::LIVE_FPS,
                quality: encode::Quality::Medium,
                output: output.clone(),
            };
            println!("正在重新封装为普通 MP4（不重新编码）…");
            let mut progress = |_done, _total| encode::Progress::Continue;
            match encode::encode_session(&dir, &opts, &mut progress) {
                Ok(_) => println!("最终视频: {}", output.display()),
                Err(e) => {
                    eprintln!("重新封装失败: {e}");
                    return 2;
                }
            }
        }
    }
    0
}

fn cmd_encode(args: &[String]) -> i32 {
    let mut dir: Option<PathBuf> = None;
    let mut fps = 10.0f64;
    let mut fps_explicit = false;
    let mut quality = encode::Quality::Medium;
    let mut out: Option<PathBuf> = None;
    let mut max_frames: Option<u32> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--session" => {
                let v = match required_value(args, &mut i, "--session") {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("{e}");
                        return 2;
                    }
                };
                dir = Some(PathBuf::from(v));
            }
            "--fps" => {
                let v = match required_value(args, &mut i, "--fps") {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("{e}");
                        return 2;
                    }
                };
                fps = match v.parse() {
                    Ok(v) => v,
                    Err(_) => {
                        eprintln!("--fps 需要数字（收到 {v}）");
                        return 2;
                    }
                };
                fps_explicit = true;
            }
            "--quality" => {
                let v = match required_value(args, &mut i, "--quality") {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("{e}");
                        return 2;
                    }
                };
                match encode::Quality::parse(&v) {
                    Some(q) => quality = q,
                    None => {
                        eprintln!("画质只支持 high/medium/low（收到 {v}）");
                        return 2;
                    }
                }
            }
            "--out" => {
                let v = match required_value(args, &mut i, "--out") {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("{e}");
                        return 2;
                    }
                };
                out = Some(PathBuf::from(v));
            }
            // 只导出前 N 帧（预览用）。它走的正是 GUI 上【取消合成】那条路径：
            // 回调返回 Cancel 后仍会写 trailer，产出可播放的部分视频。
            "--frames" => {
                let v = match required_value(args, &mut i, "--frames") {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("{e}");
                        return 2;
                    }
                };
                max_frames = match v.parse::<u32>() {
                    Ok(n) if n > 0 => Some(n),
                    _ => {
                        eprintln!("--frames 需要大于 0 的整数（收到 {v}）");
                        return 2;
                    }
                };
            }
            other => {
                eprintln!("encode 不支持参数 {other}");
                return 2;
            }
        }
        i += 1;
    }
    let dir = match dir {
        Some(d) => d,
        None => {
            eprintln!("缺少 --session DIR");
            return 2;
        }
    };
    let output = out.unwrap_or_else(|| unique_output_path(&dir, "timelapse"));
    let removed = canlapse::fsutil::cleanup_stale_temps(&dir, canlapse::fsutil::STALE_TEMP_AGE);
    if removed > 0 {
        println!("已清理 {removed} 个过期临时缓存文件");
    }
    let streaming = dir.join(canlapse::session::STREAM_FILE).is_file();
    if streaming && !fps_explicit {
        fps = encode::LIVE_FPS;
    }
    let opts = encode::EncodeOptions {
        fps,
        quality,
        output: output.clone(),
    };
    if streaming {
        println!("开始重新封装（不重新编码）：输出 {}", output.display());
    } else {
        println!(
            "开始合成：{} fps，画质 {}，输出 {}",
            fps,
            quality.label(),
            output.display()
        );
    }
    let mut last = 0u32;
    let r = encode::encode_session(&dir, &opts, &mut |done, total| {
        if done != last {
            last = done;
            if streaming {
                print!("\r重新封装 {done} 帧");
            } else {
                print!("\r编码 {done}/{total} 帧");
            }
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
        match max_frames {
            Some(n) if done >= n => encode::Progress::Cancel,
            _ => encode::Progress::Continue,
        }
    });
    match r {
        Ok(encode::EncodeOutcome::Done(s)) => {
            println!(
                "\n合成完成：{} 帧，{:.2} MB，{}x{}，{:.1} 秒\n输出: {}",
                s.frames,
                s.bytes as f64 / 1048576.0,
                s.width,
                s.height,
                s.seconds,
                output.display()
            );
            0
        }
        Ok(encode::EncodeOutcome::Cancelled(s)) => {
            println!(
                "\n合成已取消：已写入 {} 帧（{:.2} MB），文件仍是可播放的部分视频\n输出: {}",
                s.frames,
                s.bytes as f64 / 1048576.0,
                output.display()
            );
            1
        }
        Err(e) => {
            eprintln!("合成失败: {e}");
            2
        }
    }
}

fn cmd_doctor() -> i32 {
    println!("CanLapse {} 自检", env!("CARGO_PKG_VERSION"));

    println!("\n[抓帧]");
    let comp = capture::composition_enabled();
    println!(
        "  桌面合成 (DWM)：{}",
        if comp {
            "已开启 ✓"
        } else {
            "未开启 ✗ —— 普通缩略图抓帧不可用；完整画布/导航器模式仍可使用"
        }
    );

    println!("\n[编码]");
    if cfg!(ffmpeg_static) {
        println!("  ffmpeg：静态链接（构建时使用 deps/ffmpeg-static）");
    } else {
        let ffdir = canlapse::ffmpeg_min_dir();
        println!("  ffmpeg-min 目录：{}", ffdir.display());
    }
    println!(
        "  libopenh264 编码器：{}",
        if encode::encoder_available() {
            "可用 ✓"
        } else {
            "不可用 ✗"
        }
    );
    println!("  avcodec 构建配置：{}", encode::backend_info());

    if cfg!(ffmpeg_static) {
        println!("\n[运行时 DLL]");
        println!("  静态链接 ✓ 单个 exe，不需要任何 ffmpeg/VC 运行库 DLL");
    } else {
        println!("\n[运行时 DLL]（导入表在进程启动时就要解析，必须在 exe 同目录）");
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                println!("  exe 目录：{}", dir.display());
                let names: Vec<String> = std::fs::read_dir(dir)
                    .map(|rd| {
                        rd.flatten()
                            .map(|e| e.file_name().to_string_lossy().to_string())
                            .filter(|n| n.to_ascii_lowercase().ends_with(".dll"))
                            .collect()
                    })
                    .unwrap_or_default();
                for prefix in [
                    "avcodec-",
                    "avformat-",
                    "avutil-",
                    "swscale-",
                    "libwinpthread",
                ] {
                    let hit = names
                        .iter()
                        .find(|n| n.to_ascii_lowercase().starts_with(prefix));
                    match hit {
                        Some(n) => println!("  {prefix:<14} ✓ {n}"),
                        None => println!("  {prefix:<14} ✗ 缺失"),
                    }
                }
            }
        }
    }

    println!("\n[缓存]");
    println!("  缓存根目录：{}", cache_root().display());
    let sessions = session::Session::list(&cache_root());
    println!("  已有会话：{} 个", sessions.len());

    0
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // 运行日志：默认开启，写 %LOCALAPPDATA%\CanLapse\canlapse.log + exe 同目录镜像
    let log_path = canlapse::log::init();
    if args.len() < 2 {
        // 图形界面有独立入口（CanLapse.exe），所以这里没有参数就打印帮助
        help();
        std::process::exit(0);
    }
    let cmd = args[1].as_str();
    let rest: Vec<String> = args[2..].to_vec();
    canlapse::log::line(&format!(
        "==== canlapse-cli {} 启动: {} (log={:?}) ====",
        env!("CARGO_PKG_VERSION"),
        args[1..].join(" "),
        log_path
    ));

    let code = match cmd {
        "gui" => {
            let mut target: Option<isize> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--hwnd" => {
                        if let Some(v) = next_value(&rest, &mut i) {
                            target = parse_hex_hwnd(&v);
                        }
                    }
                    "--title" => {
                        if let Some(v) = next_value(&rest, &mut i) {
                            target = win::find_window(&v).map(|w| w.hwnd as isize);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            ui::run(target)
        }
        "list" | "--list" => cmd_list(),
        "pick" | "--pick" => cmd_pick(),
        "sessions" => cmd_sessions(&cache_root()),
        "doctor" => cmd_doctor(),
        "encode" => cmd_encode(&rest),
        "record" => match parse_record_args(&rest) {
            Ok(a) => cmd_record(a),
            Err(e) => {
                eprintln!("{e}");
                2
            }
        },
        "--help" | "-h" | "help" => {
            help();
            0
        }
        other => {
            eprintln!("未知命令: {other}\n");
            help();
            2
        }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record参数解析拒绝无效数字和缺失值() {
        assert!(parse_record_args(&["--fps".into(), "abc".into()]).is_err());
        assert!(parse_record_args(&["--threshold".into()]).is_err());
        assert!(parse_record_args(&["--duration".into(), "NaN".into()]).is_err());
        assert!(parse_record_args(&["--unknown".into()]).is_err());
    }

    #[test]
    fn record参数解析保留合法模式() {
        let args = parse_record_args(&[
            "--hwnd".into(),
            "0x1234".into(),
            "--fps".into(),
            "2.5".into(),
            "--threshold".into(),
            "0.006".into(),
            "--navigator".into(),
        ])
        .unwrap();
        assert_eq!(args.hwnd, Some(0x1234));
        assert_eq!(args.fps, 2.5);
        assert_eq!(args.threshold, 0.006);
        assert!(args.navigator);
    }
}
