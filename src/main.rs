//! CanLapse 命令行切片。
//!
//! 当前阶段目标是打通**垂直切片**：拾取窗口 → 按帧密度与差异阈值抓帧 → 写 PNG 缓存 →
//! （待接入）合成 MP4。GUI（460×380 单页 + 悬浮条）在切片验证通过后再做。
//!
//! 用法见 `canlapse --help`。

mod capture;
mod encode;
mod frame;
mod png_io;
mod session;
mod win;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use capture::ThumbnailCapture;
use frame::Frame;
use session::Session;

fn cache_root() -> PathBuf {
    if let Ok(p) = std::env::var("CANLAPSE_CACHE") {
        return PathBuf::from(p);
    }
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(base).join("CanLapse").join("cache")
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

fn help() {
    println!(
        r#"CanLapse —— 画布窗口缩时录屏工具（{version}）

用法:
  canlapse list                                    列出可见顶层窗口（挑目标用）
  canlapse pick                                    鼠标拾取画布窗口并打印结果
  canlapse record [目标] [选项]                     开始录制（抓帧到缓存目录）
  canlapse sessions                                列出历史缓存会话
  canlapse encode --session DIR [选项]             把会话缓存合成 MP4（尚未接入）
  canlapse --help

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
    println!("{:<12} {:<22} {:<26} {:>10}  {}", "HWND", "进程", "类名", "客户区", "标题");
    for w in &ws {
        let (cw, ch) = w.client_size();
        println!(
            "0x{:<10X} {:<22} {:<26} {:>10}  {}",
            w.hwnd as isize,
            w.process,
            w.class_name,
            format!("{cw}x{ch}"),
            w.title
        );
    }
    println!("\n共 {} 个可见顶层窗口", ws.len());
    0
}

fn cmd_pick() -> i32 {
    match win::pick_window_interactive() {
        Some(h) => {
            let info = win::describe(h);
            println!("已选中: {}", info.label());
            println!("  HWND      = 0x{:X}", h as isize);
            println!("  进程      = {}", info.process);
            println!("  类名      = {}", info.class_name);
            println!("  客户区    = {}x{}", info.client_size().0, info.client_size().1);
            println!("  已最小化  = {}", info.minimized);
            0
        }
        None => 1,
    }
}

fn cmd_sessions(root: &PathBuf) -> i32 {
    let list = Session::list(root);
    if list.is_empty() {
        println!("缓存根目录 {} 下还没有会话。", root.display());
        return 0;
    }
    println!("{:<28} {:>8}  {}", "会话", "帧数", "目录");
    for s in &list {
        println!("{:<28} {:>8}  {}", s.name, s.frames, s.dir.display());
    }
    println!("\n共 {} 个会话", list.len());
    0
}

fn resolve_target(a: &RecordArgs) -> Option<win::WindowInfo> {
    if a.pick {
        let hwnd = win::pick_window_interactive()?;
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
    let hwnd = win::pick_window_interactive()?;
    Some(win::describe(hwnd))
}

fn cmd_record(a: RecordArgs) -> i32 {
    if !capture::composition_enabled() {
        eprintln!("桌面合成（DWM）未开启，无法使用缩略图抓帧。");
        return 2;
    }
    if !(1.0..=8.0).contains(&a.fps) {
        eprintln!("帧密度必须在 1~8 之间（收到 {}）", a.fps);
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
    println!("目标: {}", info.label());

    let mut cap = match ThumbnailCapture::new(info.hwnd) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("建立抓帧失败: {e}");
            return 2;
        }
    };
    let (cw, ch) = cap.size();
    println!("抓帧就绪: {cw}x{ch}（1:1，宿主窗口在屏幕外，不影响你操作）");

    let root = a.out.clone().unwrap_or_else(cache_root);
    let name = a
        .name
        .clone()
        .unwrap_or_else(|| if info.process.is_empty() { "画布".into() } else { info.process.clone() });
    let mut sess = match Session::create(&root, &name, &info, cw, ch, a.fps, a.threshold) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    println!("会话目录: {}", sess.dir().display());
    println!(
        "录制参数: 帧密度 {}/s，差异阈值 {:.3}。按 Esc 停止{}。",
        a.fps,
        a.threshold,
        match a.duration {
            Some(d) => format!("，或 {d} 秒后自动停止"),
            None => String::new(),
        }
    );

    let interval = Duration::from_secs_f64(1.0 / a.fps);
    let start = Instant::now();
    let mut last_saved: Option<Frame> = None;
    let mut dropped: u32 = 0;
    let mut skipped: u32 = 0; // 因"画面没变"而没存帧的采样次数
    let mut blank_warned = false;
    let mut completed = true;
    let mut next_tick = Instant::now();

    loop {
        if !cap.source_alive() {
            println!("\n画布窗口已关闭，结束录制。");
            completed = false;
            break;
        }
        if win::escape_pressed() {
            println!("\n收到 Esc，结束录制。");
            break;
        }
        // 🏷 标记（规格里的悬浮条按钮；命令行阶段用 F8）
        if win::marker_key_pressed() {
            let ms = start.elapsed().as_millis() as u64;
            sess.add_marker(None, ms);
            println!(
                "\n已打标记 #{}：第 {} 帧处，{:.1} 秒",
                sess.marker_count(),
                sess.frame_count(),
                ms as f64 / 1000.0
            );
        }

        // 目标窗口尺寸变化（用户拖动了画布窗口边缘等）
        match cap.sync_source_size() {
            Ok(true) => {
                let (nw, nh) = cap.size();
                println!("\n目标客户区尺寸变为 {nw}x{nh}，已重建抓帧；上一帧作废。");
                sess.update_geometry(nw, nh);
                last_saved = None;
            }
            Ok(false) => {}
            Err(e) => {
                dropped += 1;
                if dropped <= 3 {
                    eprintln!("同步尺寸失败（跳过本次采样）: {e}");
                }
            }
        }

        match cap.grab() {
            Ok(f) => {
                if !blank_warned && f.is_blank() {
                    eprintln!(
                        "⚠ 抓到的画面是纯色。可能是缩略图失效（试试重新拾取窗口），\
                         也可能是画布当前确实是空白。"
                    );
                    blank_warned = true;
                }
                let changed = match &last_saved {
                    Some(prev) => f.changed_ratio(prev),
                    None => 1.0, // 第一帧总是存
                };
                if changed > a.threshold {
                    match sess.save_frame(&f) {
                        Ok(n) => {
                            last_saved = Some(f);
                            if !a.quiet {
                                print!(
                                    "\r已录制 {n} 帧 | 画面变化 {:.1}% | 丢弃 {dropped} | 跳过 {skipped}   ",
                                    changed * 100.0
                                );
                                use std::io::Write;
                                let _ = std::io::stdout().flush();
                            }
                        }
                        Err(e) => {
                            dropped += 1;
                            if dropped <= 3 {
                                eprintln!("保存帧失败（跳过该帧，继续录制）: {e}");
                            }
                        }
                    }
                } else {
                    skipped += 1;
                }
            }
            Err(e) => {
                // 需求：单帧失败只跳过，不中断、不崩溃
                dropped += 1;
                if dropped <= 3 || dropped % 100 == 0 {
                    eprintln!("抓帧失败（第 {dropped} 次，已跳过）: {e}");
                }
            }
        }

        // ---- 等待下一个采样点 ----
        // 关键：把等待切成 20ms 的小片来轮询按键。
        // 如果直接 sleep 整个采样间隔（低帧密度下可达 1 秒），用户短按 Esc/F8 会**被整个漏掉**。
        next_tick += interval;
        let now = Instant::now();
        let mut stop_requested = false;
        if next_tick > now {
            while Instant::now() < next_tick {
                win::pump();
                if win::escape_pressed() {
                    stop_requested = true;
                    break;
                }
                if win::marker_key_pressed() {
                    let ms = start.elapsed().as_millis() as u64;
                    sess.add_marker(None, ms);
                    println!(
                        "\n已打标记 #{}：第 {} 帧处，{:.1} 秒",
                        sess.marker_count(),
                        sess.frame_count(),
                        ms as f64 / 1000.0
                    );
                }
                let left = next_tick.saturating_duration_since(Instant::now());
                std::thread::sleep(left.min(Duration::from_millis(20)));
            }
        } else {
            next_tick = now;
        }
        if stop_requested {
            println!("\n收到 Esc，结束录制。");
            break;
        }

        if let Some(d) = a.duration {
            if d > 0.0 && start.elapsed().as_secs_f64() >= d {
                println!("\n达到指定时长 {d} 秒，结束录制。");
                break;
            }
        }
    }

    let elapsed_ms = start.elapsed().as_millis() as u64;
    if let Err(e) = sess.finish(dropped, elapsed_ms, completed) {
        eprintln!("{e}");
        return 2;
    }
    println!(
        "\n录制结束：{} 帧，{} 个标记，丢弃 {}，耗时 {:.1} 秒\n会话目录: {}",
        sess.frame_count(),
        sess.marker_count(),
        dropped,
        elapsed_ms as f64 / 1000.0,
        sess.dir().display()
    );
    println!("下一步（接入编码器后即可用）：canlapse encode --session \"{}\" --fps 10", sess.dir().display());
    0
}

fn cmd_encode(args: &[String]) -> i32 {
    let mut dir: Option<PathBuf> = None;
    let mut fps = 10.0f64;
    let mut quality = encode::Quality::High;
    let mut out: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--session" => {
                if let Some(v) = next_value(args, &mut i) {
                    dir = Some(PathBuf::from(v));
                }
            }
            "--fps" => {
                if let Some(v) = next_value(args, &mut i) {
                    fps = v.parse().unwrap_or(10.0);
                }
            }
            "--quality" => {
                if let Some(v) = next_value(args, &mut i) {
                    match encode::Quality::parse(&v) {
                        Some(q) => quality = q,
                        None => {
                            eprintln!("画质只支持 high/medium/low（收到 {v}）");
                            return 2;
                        }
                    }
                }
            }
            "--out" => {
                if let Some(v) = next_value(args, &mut i) {
                    out = Some(PathBuf::from(v));
                }
            }
            _ => {}
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
    let output = out.unwrap_or_else(|| dir.join("timelapse.mp4"));
    let opts = encode::EncodeOptions { fps, quality, output: output.clone() };
    let mut last = 0u32;
    let r = encode::encode_session(&dir, &opts, &mut |done, total| {
        if done != last {
            last = done;
            print!("\r编码 {done}/{total}");
        }
    });
    match r {
        Ok(s) => {
            println!(
                "\n完成：{} 帧，{:.1} MB，{}x{}，{:.1} 秒\n输出: {}",
                s.frames,
                s.bytes as f64 / 1048576.0,
                s.width,
                s.height,
                s.seconds,
                output.display()
            );
            0
        }
        Err(e) => {
            eprintln!("合成失败: {e}");
            2
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        help();
        std::process::exit(0);
    }
    let cmd = args[1].as_str();
    let rest: Vec<String> = args[2..].to_vec();

    let code = match cmd {
        "list" | "--list" => cmd_list(),
        "pick" | "--pick" => cmd_pick(),
        "sessions" => cmd_sessions(&cache_root()),
        "encode" => cmd_encode(&rest),
        "record" => {
            let mut a = RecordArgs::default();
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--hwnd" => {
                        if let Some(v) = next_value(&rest, &mut i) {
                            match parse_hex_hwnd(&v) {
                                Some(h) => a.hwnd = Some(h),
                                None => {
                                    eprintln!("--hwnd 需要十六进制句柄，例如 0x00120AB4");
                                    std::process::exit(2);
                                }
                            }
                        }
                    }
                    "--title" => {
                        if let Some(v) = next_value(&rest, &mut i) {
                            a.title = Some(v);
                        }
                    }
                    "--pick" => a.pick = true,
                    "--out" => {
                        if let Some(v) = next_value(&rest, &mut i) {
                            a.out = Some(PathBuf::from(v));
                        }
                    }
                    "--name" => {
                        if let Some(v) = next_value(&rest, &mut i) {
                            a.name = Some(v);
                        }
                    }
                    "--fps" => {
                        if let Some(v) = next_value(&rest, &mut i) {
                            a.fps = v.parse().unwrap_or(a.fps);
                        }
                    }
                    "--threshold" => {
                        if let Some(v) = next_value(&rest, &mut i) {
                            a.threshold = v.parse().unwrap_or(a.threshold);
                        }
                    }
                    "--duration" => {
                        if let Some(v) = next_value(&rest, &mut i) {
                            a.duration = v.parse().ok();
                        }
                    }
                    "--quiet" => a.quiet = true,
                    _ => {}
                }
                i += 1;
            }
            cmd_record(a)
        }
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
