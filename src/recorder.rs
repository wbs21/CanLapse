//! 录制工作线程：抓帧 → 差分 → 分辨率统一 → 落盘。与 UI 完全解耦。
//!
//! 为什么必须是独立线程：
//! 1. GDI 的 DC/位图有**线程亲和性**，`ThumbnailCapture` 必须在同一个线程里创建、抓帧、销毁；
//! 2. UI 线程不能被抓帧阻塞 —— 4K 画布一帧就有 33 MB 要拷，可能耗几十毫秒。
//!
//! 线程与外界只通过 [`Shared`] 里的原子量通信，因此暂停/标记/停止都是即时的，
//! 不需要等抓帧循环走到某个检查点。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::HWND;

use crate::capture::ThumbnailCapture;
use crate::frame::Frame;
use crate::logline;
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
    /// 是否启用"其他软件在前台就暂停抓帧"。
    ///
    /// GUI 流程用 `true`（用户点完录制会点进画布，规则正是为此设计）。
    /// 但**命令行/脚本**常常是"在终端里录一个后台窗口"，此时前台永远是终端，
    /// 规则会让录制一直暂停、一帧都不出 —— 所以这类场景要能关掉。
    pub fg_pause: bool,
}

pub struct Shared {
    phase: AtomicU32,
    frames: AtomicU32,
    dropped: AtomicU32,
    markers: AtomicU32,
    elapsed_ms: AtomicU64,
    base_w: AtomicI32,
    base_h: AtomicI32,
    /// 因为"别的软件在前台"而暂停抓帧（说明此刻没在画画）
    fg_paused: AtomicBool,
    stop: AtomicBool,
    paused: AtomicBool,
    /// 待写入的标记时间点（毫秒，已扣除暂停）。用队列而不是 bool：
    /// 低帧密度下用户可能连续点两次标记，bool 会把前一次覆盖掉。
    marks: Mutex<VecDeque<u64>>,
    /// 控制面等待：Stop/Mark/Pause 通过 Condvar 立即唤醒录制线程，
    /// 采样间隔足够长时不再每秒醒来 50 次。
    control: Mutex<()>,
    control_cv: Condvar,
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
            fg_paused: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            marks: Mutex::new(VecDeque::new()),
            control: Mutex::new(()),
            control_cv: Condvar::new(),
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

    /// 是否因为"别的软件在前台"而暂停抓帧。
    /// 用户要求：别的软件在前台 => 此刻没在画画 => 不必录（切回绘画软件就继续）。
    pub fn fg_paused(&self) -> bool {
        self.fg_paused.load(Ordering::SeqCst)
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
        self.control_cv.notify_all();
    }

    /// 用户已经点了停止（录制线程可能还在跑最后一帧）
    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
    /// 切换暂停，返回切换后的暂停状态
    pub fn toggle_pause(&self) -> bool {
        let now = !self.paused.load(Ordering::SeqCst);
        self.paused.store(now, Ordering::SeqCst);
        self.control_cv.notify_all();
        now
    }
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }
    /// 打一个标记。
    ///
    /// 时间点在这里（点击时）取，而不是等录制线程醒来再取 —— 默认 0.2fps 时
    /// 后者会让标记晚最多 5 秒。`elapsed_ms` 由录制线程持续刷新，暂停时冻结，
    /// 因此暂停期间点标记也能得到“暂停那一刻”的合理时间。
    pub fn request_mark(&self) {
        let ms = self.elapsed_ms.load(Ordering::SeqCst);
        if let Ok(mut q) = self.marks.lock() {
            q.push_back(ms);
        }
        self.control_cv.notify_all();
    }

    /// 等待控制事件或超时。返回后调用方重新检查 stop/mark/paused。
    pub fn wait_control(&self, timeout: Duration) {
        if let Ok(guard) = self.control.lock() {
            let _ = self.control_cv.wait_timeout(guard, timeout);
        } else {
            std::thread::sleep(timeout);
        }
    }

    /// 是否抓到过纯色画面（多半是缩略图失效 —— 见 ADR 0002 的"依赖实现行为"风险）。
    /// UI 应当据此给出告警，而不是静默录出一段纯色视频。
    pub fn blank_warned(&self) -> bool {
        self.blank_warned.load(Ordering::SeqCst)
    }

    /// 取走全部待处理标记。返回空表示没有新标记。
    fn take_marks(&self) -> Vec<u64> {
        match self.marks.lock() {
            Ok(mut q) => q.drain(..).collect(),
            Err(_) => Vec::new(),
        }
    }
}

enum WriterCmd {
    Frame(Frame),
    Marker {
        elapsed_ms: u64,
    },
    Geometry {
        width: i32,
        height: i32,
    },
    Finish {
        dropped: u32,
        duration_ms: u64,
        completed: bool,
        ack: SyncSender<Result<(), String>>,
    },
}

/// 把 JPEG/PNG 编码和磁盘写入移出抓帧线程。
///
/// 抓帧线程只负责抓帧、差分和把变化帧放进有界队列；写盘线程复用同一个
/// `Session`，按队列顺序保存帧/标记，停止时 drain 完再写最终的 markers.json。
struct SessionWriter {
    tx: Option<SyncSender<WriterCmd>>,
    handle: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
    pool: Arc<Mutex<Vec<Frame>>>,
}

fn join_writer_timeout(handle: Option<JoinHandle<()>>, timeout: Duration) {
    let Some(handle) = handle else {
        return;
    };
    let deadline = Instant::now() + timeout;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            logline!("[record] writer 线程收尾超时，放弃等待");
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = handle.join();
}

impl SessionWriter {
    fn new(mut session: Session, shared: Arc<Shared>) -> Self {
        let (tx, rx) = sync_channel::<WriterCmd>(4);
        let shared_for_thread = shared.clone();
        let pool = Arc::new(Mutex::new(Vec::<Frame>::new()));
        let pool_for_thread = pool.clone();
        let handle = std::thread::spawn(move || {
            // JPEG/写盘是后台工作，降一档优先级，避免和绘画软件争 CPU。
            unsafe {
                windows_sys::Win32::System::Threading::SetThreadPriority(
                    windows_sys::Win32::System::Threading::GetCurrentThread(),
                    -1, // THREAD_PRIORITY_BELOW_NORMAL
                );
            }
            while let Ok(cmd) = rx.recv() {
                match cmd {
                    WriterCmd::Frame(frame) => {
                        // 单次写失败重试两次；仍失败时停止录制。异步 writer 下
                        // prev 已推进到这一帧，继续比较会把未落盘帧当成上一帧，
                        // 造成静默丢帧。
                        let mut result = session.save_frame(&frame);
                        for _ in 0..2 {
                            if result.is_ok() {
                                break;
                            }
                            result = session.save_frame(&frame);
                        }
                        match result {
                            Ok(n) => {
                                shared_for_thread.frames.store(n, Ordering::SeqCst);
                            }
                            Err(e) => {
                                shared_for_thread.dropped.fetch_add(1, Ordering::SeqCst);
                                logline!("[record] writer 保存帧失败：{e}");
                                if let Ok(mut slot) = shared_for_thread.error.lock() {
                                    *slot = Some(format!("保存帧失败（已重试 2 次）: {e}"));
                                }
                                shared_for_thread.request_stop();
                            }
                        }
                        // 把 Frame 归还缓冲池，避免每次保存都重新分配几十 MB。
                        if let Ok(mut pool) = pool_for_thread.lock() {
                            if pool.len() < 4 {
                                pool.push(frame);
                            }
                        }
                    }
                    WriterCmd::Marker { elapsed_ms } => {
                        session.add_marker(None, elapsed_ms);
                        shared_for_thread
                            .markers
                            .store(session.marker_count() as u32, Ordering::SeqCst);
                    }
                    WriterCmd::Geometry { width, height } => {
                        session.update_geometry(width, height);
                    }
                    WriterCmd::Finish {
                        dropped,
                        duration_ms,
                        completed,
                        ack,
                    } => {
                        let result = session.finish(dropped, duration_ms, completed);
                        let _ = ack.send(result);
                        break;
                    }
                }
            }
        });
        Self {
            tx: Some(tx),
            handle: Some(handle),
            shared,
            pool,
        }
    }

    /// 非阻塞入队；队列满时丢弃该帧，保证抓帧线程不被磁盘拖住。
    fn save(&self, frame: &Frame) -> bool {
        let Some(tx) = &self.tx else {
            return false;
        };
        let mut job = {
            let mut pool = self
                .pool
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            pool.pop()
        };
        let job = match job.take() {
            Some(mut f)
                if f.width == frame.width
                    && f.height == frame.height
                    && f.bgra.len() == frame.bgra.len() =>
            {
                f.bgra.copy_from_slice(&frame.bgra);
                f
            }
            _ => Frame::new(frame.width, frame.height, frame.bgra.clone()),
        };
        match tx.try_send(WriterCmd::Frame(job)) {
            Ok(()) => true,
            Err(TrySendError::Full(WriterCmd::Frame(job)))
            | Err(TrySendError::Disconnected(WriterCmd::Frame(job))) => {
                if let Ok(mut pool) = self.pool.lock() {
                    if pool.len() < 4 {
                        pool.push(job);
                    }
                }
                self.shared.dropped.fetch_add(1, Ordering::SeqCst);
                false
            }
            Err(_) => false,
        }
    }

    fn marker(&self, elapsed_ms: u64) {
        if let Some(tx) = &self.tx {
            // 标记数量很少，允许短暂等待，避免丢失用户点击。
            let _ = tx.send(WriterCmd::Marker { elapsed_ms });
        }
    }

    fn set_geometry(&self, width: i32, height: i32) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(WriterCmd::Geometry { width, height });
        }
    }

    fn finish(mut self, dropped: u32, duration_ms: u64, completed: bool) -> Result<(), String> {
        let (ack_tx, ack_rx) = sync_channel(1);
        let Some(tx) = self.tx.take() else {
            return Ok(());
        };
        tx.send(WriterCmd::Finish {
            dropped,
            duration_ms,
            completed,
            ack: ack_tx,
        })
        .map_err(|_| "写入线程已退出".to_string())?;
        drop(tx);
        let result = match ack_rx.recv_timeout(Duration::from_secs(30)) {
            Ok(r) => r,
            Err(_) => Err("等待写入线程收尾超时".to_string()),
        };
        join_writer_timeout(self.handle.take(), Duration::from_secs(5));
        result
    }
}

impl Drop for SessionWriter {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let (ack_tx, _) = sync_channel(1);
            let _ = tx.send(WriterCmd::Finish {
                dropped: self.shared.dropped.load(Ordering::SeqCst),
                duration_ms: self.shared.elapsed_ms.load(Ordering::SeqCst),
                completed: false,
                ack: ack_tx,
            });
        }
        join_writer_timeout(self.handle.take(), Duration::from_secs(5));
    }
}

/// 启动录制线程。返回的 `JoinHandle` 只用于等待收尾，进度一律看 [`Shared`]。
pub fn spawn(cfg: RecorderConfig, shared: Arc<Shared>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        // 线程里 panic 的话 UI 只会看到"一直录制中"——必须接住并转成错误
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&cfg, &shared)));
        let err = match outcome {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(e),
            Err(p) => Some(format!("录制线程 panic：{}", panic_text(&p))),
        };
        if let Some(e) = err {
            crate::log::line(&format!("[record] 录制线程异常：{e}"));
            if let Ok(mut slot) = shared.error.lock() {
                *slot = Some(e);
            }
            shared.phase.store(Phase::Failed.code(), Ordering::SeqCst);
        }
    })
}

fn panic_text(e: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = e.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = e.downcast_ref::<String>() {
        s.clone()
    } else {
        "未知 panic".to_string()
    }
}

fn apply_marks(shared: &Shared, writer: &SessionWriter) {
    let marks = shared.take_marks();
    if marks.is_empty() {
        return;
    }
    for ms in marks {
        writer.marker(ms);
    }
}

fn run(cfg: &RecorderConfig, shared: &Arc<Shared>) -> Result<(), String> {
    let hwnd = cfg.hwnd as HWND;
    let info = win::describe(hwnd);
    let target_pid = win::pid_of(hwnd);
    // 注意：ThumbnailCapture 在这个线程里创建，之后也只在它上面用（GDI 线程亲和性）
    let mut cap = ThumbnailCapture::new(hwnd)?;
    let (cw, ch) = cap.size();
    let sess = Session::create(&cfg.root, &cfg.name, &info, cw, ch, cfg.fps, cfg.threshold)?;
    let session_dir = sess.dir().to_path_buf();
    let writer = SessionWriter::new(sess, shared.clone());
    if let Ok(mut d) = shared.dir.lock() {
        *d = Some(session_dir);
    }
    shared
        .phase
        .store(Phase::Recording.code(), Ordering::SeqCst);

    let interval = Duration::from_secs_f64(1.0 / cfg.fps.max(0.1));
    let start = Instant::now();
    let mut paused_total = Duration::ZERO;
    let mut paused_at: Option<Instant> = None;
    // 差分始终用**原始捕获帧**；落盘用等比适配到基准尺寸后的图。
    // `cur` / `prev` 两块缓冲交替使用：抓帧写进 cur，保存成功后与 prev 交换 ——
    // 这样每帧不再分配/释放 7.5 MB（4K 时是 33 MB），长时间录制内存曲线是平的。
    let mut cur = Frame::new(
        cw,
        ch,
        vec![
            0u8;
            (cw as usize)
                .saturating_mul(ch as usize)
                .saturating_mul(4)
                .max(4)
        ],
    );
    let mut prev: Option<Frame> = None;
    let mut base_size: Option<(i32, i32)> = None;
    let mut next_tick = Instant::now();
    let mut completed = true;
    let mut tick: u32 = 0;
    // 抓帧耗时统计（每 40 个 tick 记一条日志，用来验证"复用缓冲"的效果）
    let mut grab_us: u64 = 0;
    let mut grab_n: u32 = 0;
    let mut grab_failures: u32 = 0;

    loop {
        // 先落标记再判断停止：用户点完标记马上点停止时，不能把刚点的标记丢掉。
        apply_marks(shared, &writer);
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        if !cap.source_alive() {
            completed = false;
            break;
        }

        if shared.paused.load(Ordering::SeqCst) || {
            // 用户规则：别的软件在前台 => 说明绘画软件此刻挂起、没有新笔触，不必抓帧。
            // 我们自己的进程不算（悬浮条是 WS_EX_NOACTIVATE，本来也不会成为前台）。
            let fg = win::foreground_pid();
            let other = cfg.fg_pause && fg != 0 && fg != target_pid && fg != std::process::id();
            shared.fg_paused.store(other, Ordering::SeqCst);
            other
        } {
            if paused_at.is_none() {
                paused_at = Some(Instant::now());
                shared.phase.store(Phase::Paused.code(), Ordering::SeqCst);
            }
            // 暂停时不需要 20Hz 轮询；200ms 一次足够判断前台窗口变化，
            // Stop/Mark 会通过 Condvar 立即唤醒。
            shared.wait_control(Duration::from_millis(200));
            continue;
        } else if let Some(t) = paused_at.take() {
            paused_total += t.elapsed();
            shared
                .phase
                .store(Phase::Recording.code(), Ordering::SeqCst);
        }

        // 目标窗口尺寸变化：重建抓帧（基准分辨率一旦确定就不再改）
        match cap.sync_source_size() {
            Ok(true) => {
                let (nw, nh) = cap.size();
                if base_size.is_none() {
                    writer.set_geometry(nw, nh);
                }
                prev = None; // 尺寸变了，上一帧作废（强制下一帧保存）
            }
            Ok(false) => {}
            Err(e) => {
                shared.dropped.fetch_add(1, Ordering::SeqCst);
                grab_failures += 1;
                if grab_failures >= 30 {
                    return Err(format!("连续 {grab_failures} 次抓帧同步失败: {e}"));
                }
            }
        }

        let t_grab = Instant::now();
        let grabbed = match &prev {
            Some(p) => cap.grab_if_changed(p, cfg.threshold, &mut cur),
            None => cap.grab_into(&mut cur).map(|_| true),
        };
        grab_us += t_grab.elapsed().as_micros() as u64;
        grab_n += 1;
        match grabbed {
            Ok(changed) => {
                grab_failures = 0;
                if changed {
                    if shared.blank_warned.load(Ordering::SeqCst) {
                        // 一旦出现真实内容就解除告警，避免“曾经空白”永久挂在悬浮条上。
                        if !cur.is_blank() {
                            shared.blank_warned.store(false, Ordering::SeqCst);
                        }
                    } else if cur.is_blank() {
                        shared.blank_warned.store(true, Ordering::SeqCst);
                    }
                    let (bw, bh) = match base_size {
                        None => {
                            base_size = Some((cur.width, cur.height));
                            writer.set_geometry(cur.width, cur.height);
                            shared.base_w.store(cur.width, Ordering::SeqCst);
                            shared.base_h.store(cur.height, Ordering::SeqCst);
                            (cur.width, cur.height)
                        }
                        Some(sz) => sz,
                    };
                    // 尺寸一致时直接存 cur（不 clone）；不一致才做等比适配
                    let fitted;
                    let to_save: &Frame = if (cur.width, cur.height) == (bw, bh) {
                        &cur
                    } else {
                        fitted = cur.fit_into(bw, bh, cfg.fill_rgb);
                        &fitted
                    };
                    // 交给 writer 线程编码/落盘；队列满时由 writer 计数 dropped，
                    // 这里不交换缓冲，下一轮仍拿当前画面重试。
                    if writer.save(to_save) {
                        // 交换缓冲：prev 拿走刚保存的帧，cur 拿旧缓冲继续写
                        let old_prev = prev.take();
                        let next_cur = match old_prev {
                            Some(p) if p.width == cur.width && p.height == cur.height => p,
                            _ => Frame::new(cur.width, cur.height, vec![0u8; cur.bgra.len()]),
                        };
                        prev = Some(std::mem::replace(&mut cur, next_cur));
                    }
                }
            }
            // 单帧失败只跳过，不中断录制
            Err(e) => {
                shared.dropped.fetch_add(1, Ordering::SeqCst);
                grab_failures += 1;
                if grab_failures >= 30 {
                    return Err(format!("连续 {grab_failures} 次抓帧失败: {e}"));
                }
            }
        }

        // 遮挡统计当前没有 UI 消费者；不要周期性枚举所有顶层窗口。
        tick = tick.wrapping_add(1);

        let elapsed = start.elapsed().saturating_sub(paused_total);
        shared
            .elapsed_ms
            .store(elapsed.as_millis() as u64, Ordering::SeqCst);

        if tick % 40 == 0 {
            let avg = if grab_n > 0 {
                grab_us as f64 / grab_n as f64 / 1000.0
            } else {
                0.0
            };
            logline!(
                "[perf] 抓帧 {grab_n} 次，平均 {avg:.2} ms/帧，丢弃 {}",
                shared.dropped.load(Ordering::SeqCst)
            );
            grab_us = 0;
            grab_n = 0;
        }

        next_tick += interval;
        let now = Instant::now();
        if next_tick > now {
            // 不要一次睡满整个采样间隔。Stop / 暂停 / 标记都只在循环里检查；
            // 默认 0.2fps 下一次 sleep 5 秒，会让“停止”像卡住一样。
            // 切成 20ms 小片，同时持续刷新计时并处理标记。
            let mut left = next_tick - now;
            while !left.is_zero() {
                // 最长 200ms 醒一次；控制事件会提前 notify 唤醒。
                shared.wait_control(left.min(Duration::from_millis(200)));
                let active = start.elapsed().saturating_sub(paused_total);
                shared
                    .elapsed_ms
                    .store(active.as_millis() as u64, Ordering::SeqCst);
                apply_marks(shared, &writer);
                if shared.stop.load(Ordering::SeqCst) || shared.paused.load(Ordering::SeqCst) {
                    break;
                }
                let now = Instant::now();
                left = next_tick.saturating_duration_since(now);
            }
        } else {
            next_tick = now; // 落后就重置基准，不追赶
        }
    }

    if let Some(t) = paused_at.take() {
        paused_total += t.elapsed();
    }
    let elapsed = start.elapsed().saturating_sub(paused_total);
    let dropped = shared.dropped.load(Ordering::SeqCst);
    writer.finish(dropped, elapsed.as_millis() as u64, completed)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 连续标记不会互相覆盖() {
        let s = Shared::new();
        s.elapsed_ms.store(120, Ordering::SeqCst);
        s.request_mark();
        s.elapsed_ms.store(340, Ordering::SeqCst);
        s.request_mark();
        assert_eq!(s.take_marks(), vec![120, 340]);
        assert!(s.take_marks().is_empty());
    }

    #[test]
    fn writer按顺序保存帧与标记() {
        let root = std::path::PathBuf::from("target")
            .join("test-tmp")
            .join(format!("canlapse-writer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let info = win::WindowInfo {
            hwnd: std::ptr::null_mut(),
            title: "writer".into(),
            class_name: "Test".into(),
            pid: 0,
            process: "test.exe".into(),
            rect: windows_sys::Win32::Foundation::RECT {
                left: 0,
                top: 0,
                right: 4,
                bottom: 4,
            },
            client: windows_sys::Win32::Foundation::RECT {
                left: 0,
                top: 0,
                right: 4,
                bottom: 4,
            },
            minimized: false,
        };
        let session = Session::create(&root, "writer", &info, 4, 4, 1.0, 0.01).unwrap();
        let shared = Shared::new();
        let writer = SessionWriter::new(session, shared.clone());
        let frame = Frame::new(4, 4, vec![0u8; 64]);
        assert!(writer.save(&frame));
        writer.marker(123);
        assert!(writer.save(&frame));
        writer.finish(0, 456, true).unwrap();
        assert_eq!(shared.frames.load(Ordering::SeqCst), 2);
        let list = Session::list(&root);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].frames, 2);
        let text = std::fs::read_to_string(list[0].dir.join("markers.json")).unwrap();
        let parsed: crate::session::MarkersFile = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.markers.len(), 1);
        assert_eq!(parsed.markers[0].elapsed_ms, 123);
        assert!(parsed.session.completed);
        let _ = std::fs::remove_dir_all(&root);
    }
}
