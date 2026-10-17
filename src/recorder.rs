//! 录制工作线程：抓帧 → 差分 → 分辨率统一 → 落盘。与 UI 完全解耦。
//!
//! 为什么必须是独立线程：
//! 1. GDI 的 DC/位图有**线程亲和性**，`ThumbnailCapture` 必须在同一个线程里创建、抓帧、销毁；
//! 2. UI 线程不能被抓帧阻塞 —— 4K 画布一帧就有 33 MB 要拷，可能耗几十毫秒。
//!
//! 线程与外界只通过 [`Shared`] 里的原子量通信，因此暂停/标记/停止都是即时的，
//! 不需要等抓帧循环走到某个检查点。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::HWND;

use crate::capture::ThumbnailCapture;
use crate::frame::Frame;
use crate::session::Session;
use crate::win;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Idle,
    Recording,
    Paused,
    Finished,
    Failed,
}

impl Phase {
    fn code(self) -> u32 {
        match self {
            Phase::Idle => 0,
            Phase::Recording => 1,
            Phase::Paused => 2,
            Phase::Finished => 3,
            Phase::Failed => 4,
        }
    }
    fn from_code(c: u32) -> Phase {
        match c {
            1 => Phase::Recording,
            2 => Phase::Paused,
            3 => Phase::Finished,
            4 => Phase::Failed,
            _ => Phase::Idle,
        }
    }
    pub fn is_running(self) -> bool {
        matches!(self, Phase::Recording | Phase::Paused)
    }
}

#[derive(Clone)]
pub struct RecorderConfig {
    pub hwnd: isize,
    pub root: PathBuf,
    pub name: String,
    /// 帧密度（快照采样频率）1~8
    pub fps: f64,
    /// 像素差异阈值 0.00~0.05
    pub threshold: f32,
    /// 分辨率统一时的填充色（默认黑边）
    pub fill_rgb: [u8; 3],
}

pub struct Shared {
    phase: AtomicU32,
    frames: AtomicU32,
    dropped: AtomicU32,
    markers: AtomicU32,
    elapsed_ms: AtomicU64,
    base_w: AtomicI32,
    base_h: AtomicI32,
    /// 画布被同属主面板遮挡的百分比（0~100），动态刷新
    covered_pct: AtomicU32,
    stop: AtomicBool,
    paused: AtomicBool,
    mark: AtomicBool,
    blank_warned: AtomicBool,
    dir: Mutex<Option<PathBuf>>,
    error: Mutex<Option<String>>,
}

impl Shared {
    pub fn new() -> Arc<Shared> {
        Arc::new(Shared {
            phase: AtomicU32::new(Phase::Idle.code()),
            frames: AtomicU32::new(0),
            dropped: AtomicU32::new(0),
            markers: AtomicU32::new(0),
            elapsed_ms: AtomicU64::new(0),
            base_w: AtomicI32::new(0),
            base_h: AtomicI32::new(0),
            covered_pct: AtomicU32::new(0),
            stop: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            mark: AtomicBool::new(false),
            blank_warned: AtomicBool::new(false),
            dir: Mutex::new(None),
            error: Mutex::new(None),
        })
    }

    pub fn phase(&self) -> Phase {
        Phase::from_code(self.phase.load(Ordering::SeqCst))
    }
    pub fn frames(&self) -> u32 {
        self.frames.load(Ordering::SeqCst)
    }
    pub fn dropped(&self) -> u32 {
        self.dropped.load(Ordering::SeqCst)
    }
    pub fn markers(&self) -> u32 {
        self.markers.load(Ordering::SeqCst)
    }
    pub fn elapsed_ms(&self) -> u64 {
        self.elapsed_ms.load(Ordering::SeqCst)
    }
    /// 基准分辨率（0x0 表示还没确定）
    pub fn base_size(&self) -> (i32, i32) {
        (
            self.base_w.load(Ordering::SeqCst),
            self.base_h.load(Ordering::SeqCst),
        )
    }

    /// 画布被自家面板遮挡的百分比（0~100）。>0 表示画面里混进了面板 —— 详见
    /// [`crate::win::covered_ratio`]：被盖住的画布像素是拿不回来的，只能提醒。
    pub fn covered_pct(&self) -> u32 {
        self.covered_pct.load(Ordering::SeqCst)
    }
    pub fn session_dir(&self) -> Option<PathBuf> {
        self.dir.lock().ok().and_then(|d| d.clone())
    }
    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|e| e.clone())
    }

    /// 请求停止（线程会在本轮循环结束时收尾并写 markers.json）
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
    /// 切换暂停，返回切换后的暂停状态
    pub fn toggle_pause(&self) -> bool {
        let now = !self.paused.load(Ordering::SeqCst);
        self.paused.store(now, Ordering::SeqCst);
        now
    }
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }
    /// 打一个标记（线程会在下次循环处理）
    pub fn request_mark(&self) {
        self.mark.store(true, Ordering::SeqCst);
    }

    /// 是否抓到过纯色画面（多半是缩略图失效 —— 见 ADR 0002 的"依赖实现行为"风险）。
    /// UI 应当据此给出告警，而不是静默录出一段纯色视频。
    pub fn blank_warned(&self) -> bool {
        self.blank_warned.load(Ordering::SeqCst)
    }

    fn mark_handled(&self) -> bool {
        self.mark.swap(false, Ordering::SeqCst)
    }
}

/// 启动录制线程。返回的 `JoinHandle` 只用于等待收尾，进度一律看 [`Shared`]。
pub fn spawn(cfg: RecorderConfig, shared: Arc<Shared>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        if let Err(e) = run(&cfg, &shared) {
            if let Ok(mut slot) = shared.error.lock() {
                *slot = Some(e);
            }
            shared.phase.store(Phase::Failed.code(), Ordering::SeqCst);
        }
    })
}

fn run(cfg: &RecorderConfig, shared: &Arc<Shared>) -> Result<(), String> {
    let hwnd = cfg.hwnd as HWND;
    let info = win::describe(hwnd);
    // 注意：ThumbnailCapture 在这个线程里创建，之后也只在它上面用（GDI 线程亲和性）
    let mut cap = ThumbnailCapture::new(hwnd)?;
    let (cw, ch) = cap.size();
    let mut sess = Session::create(
        &cfg.root,
        &cfg.name,
        &info,
        cw,
        ch,
        cfg.fps,
        cfg.threshold,
    )?;
    if let Ok(mut d) = shared.dir.lock() {
        *d = Some(sess.dir().to_path_buf());
    }
    shared.phase.store(Phase::Recording.code(), Ordering::SeqCst);

    let interval = Duration::from_secs_f64(1.0 / cfg.fps.max(0.1));
    let start = Instant::now();
    let mut paused_total = Duration::ZERO;
    let mut paused_at: Option<Instant> = None;
    // 差分始终用**原始捕获帧**；落盘用等比适配到基准尺寸后的图
    let mut last_saved: Option<Frame> = None;
    let mut base_size: Option<(i32, i32)> = None;
    let mut next_tick = Instant::now();
    let mut completed = true;
    let mut tick: u32 = 0;

    loop {
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        if !cap.source_alive() {
            completed = false;
            break;
        }

        if shared.paused.load(Ordering::SeqCst) {
            if paused_at.is_none() {
                paused_at = Some(Instant::now());
                shared.phase.store(Phase::Paused.code(), Ordering::SeqCst);
            }
            std::thread::sleep(Duration::from_millis(50));
            continue;
        } else if let Some(t) = paused_at.take() {
            paused_total += t.elapsed();
            shared.phase.store(Phase::Recording.code(), Ordering::SeqCst);
        }

        if shared.mark_handled() {
            let ms = start.elapsed().saturating_sub(paused_total).as_millis() as u64;
            sess.add_marker(None, ms);
            shared.markers.store(sess.marker_count() as u32, Ordering::SeqCst);
        }

        // 目标窗口尺寸变化：重建抓帧（基准分辨率一旦确定就不再改）
        match cap.sync_source_size() {
            Ok(true) => {
                let (nw, nh) = cap.size();
                if base_size.is_none() {
                    sess.update_geometry(nw, nh);
                }
                last_saved = None; // 强制下一帧保存
            }
            Ok(false) => {}
            Err(_) => {
                shared.dropped.fetch_add(1, Ordering::SeqCst);
            }
        }

        match cap.grab() {
            Ok(f) => {
                if !shared.blank_warned.load(Ordering::SeqCst) && f.is_blank() {
                    shared.blank_warned.store(true, Ordering::SeqCst);
                }
                let changed = match &last_saved {
                    Some(prev) => f.changed_ratio(prev),
                    None => 1.0,
                };
                if changed > cfg.threshold {
                    let to_save = match base_size {
                        None => {
                            base_size = Some((f.width, f.height));
                            sess.update_geometry(f.width, f.height);
                            shared.base_w.store(f.width, Ordering::SeqCst);
                            shared.base_h.store(f.height, Ordering::SeqCst);
                            f.clone()
                        }
                        Some((bw, bh)) => f.fit_into(bw, bh, cfg.fill_rgb),
                    };
                    match sess.save_frame(&to_save) {
                        Ok(n) => {
                            last_saved = Some(f);
                            shared.frames.store(n, Ordering::SeqCst);
                        }
                        Err(_) => {
                            shared.dropped.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
            }
            // 单帧失败只跳过，不中断录制
            Err(_) => {
                shared.dropped.fetch_add(1, Ordering::SeqCst);
            }
        }

        // 每 4 次采样统计一次遮挡（枚举窗口有成本，不必每帧都做）。
        // 动态刷新：用户拖动面板时这个数字要跟着变。
        tick = tick.wrapping_add(1);
        if tick % 4 == 0 {
            let pct = (cap.covered_ratio() * 100.0).round().clamp(0.0, 100.0) as u32;
            shared.covered_pct.store(pct, Ordering::SeqCst);
        }

        let elapsed = start.elapsed().saturating_sub(paused_total);
        shared
            .elapsed_ms
            .store(elapsed.as_millis() as u64, Ordering::SeqCst);

        next_tick += interval;
        let now = Instant::now();
        if next_tick > now {
            std::thread::sleep(next_tick - now);
        } else {
            next_tick = now; // 落后就重置基准，不追赶
        }
    }

    if let Some(t) = paused_at.take() {
        paused_total += t.elapsed();
    }
    let elapsed = start.elapsed().saturating_sub(paused_total);
    let dropped = shared.dropped.load(Ordering::SeqCst);
    sess.finish(dropped, elapsed.as_millis() as u64, completed)?;
    shared
        .elapsed_ms
        .store(elapsed.as_millis() as u64, Ordering::SeqCst);
    shared.phase.store(Phase::Finished.code(), Ordering::SeqCst);
    Ok(())
}

/// 把毫秒格式化成 `mm:ss`（悬浮条与界面都用）
pub fn format_duration(ms: u64) -> String {
    let total = ms / 1000;
    format!("{:02}:{:02}", total / 60, total % 60)
}
