//! CanLapse 单页主界面（原生 Win32，无第三方 UI 框架）。
//!
//! 布局与交互严格按 `docs/requirements.md`：
//! 顶部软件名 → 拾取模块（按钮组 + 160×100 实时预览 + 状态提示）→ 录制参数（折叠，默认展开）
//! → 导出视频参数（折叠，默认折叠）→ 历史缓存会话（折叠，默认折叠）→ 底部操作区（常驻，随状态切换）。
//!
//! 两处相对规格的**有意偏差**（会在 CHANGELOG 里写明）：
//! 1. 历史会话用「列表 + 三个按钮」而不是"每行三个按钮" —— 460px 宽的窗口里每行塞三个按钮不可用；
//! 2. 面板展开导致内容超过窗口高度时，窗口会**自动长高**（规格给的最小尺寸 400×320 也说明窗口可变），
//!    而不是把控件裁掉。

mod dropdown;
mod floatbar;
mod scale;
mod theme;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreatePen, DeleteObject, EndPaint, GetStockObject, InvalidateRect, RoundRect,
    SelectObject, SetBkMode, StretchDIBits, UpdateWindow, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HGDIOBJ, NULL_BRUSH, PAINTSTRUCT, PS_SOLID, RGBQUAD, SRCCOPY,
};
use windows_sys::Win32::UI::Controls::{
    InitCommonControlsEx, ICC_BAR_CLASSES, ICC_PROGRESS_CLASS, ICC_STANDARD_CLASSES,
    INITCOMMONCONTROLSEX, PBM_SETPOS, PBM_SETRANGE32,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetWindowLongPtrW, GetWindowRect, LoadCursorW, MessageBoxW, MoveWindow, PostQuitMessage,
    RegisterClassW, SendMessageW, SetCursor, SetForegroundWindow, SetTimer, SetWindowLongPtrW,
    SetWindowPos, SetWindowTextW, ShowWindow, TranslateMessage, GWLP_USERDATA, MB_ICONINFORMATION,
    MB_OK, MSG, SWP_NOZORDER, SW_MINIMIZE, SW_RESTORE, SW_SHOW, WM_CLOSE, WM_COMMAND, WM_CREATE,
    WM_DESTROY, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_HSCROLL, WM_LBUTTONUP, WM_PAINT, WM_SETCURSOR,
    WM_SIZE, WM_TIMER, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
    WS_THICKFRAME, WS_VISIBLE,
};

use crate::capture::ThumbnailCapture;
use crate::encode::{self, EncodeOptions, EncodeOutcome, Progress, Quality};
use crate::frame::Frame;
use crate::lang;
use crate::logline;
use crate::recorder::{self, Phase, RecorderConfig, Shared};
use crate::session::Session;
use crate::win;

// ---------------------------------------------------------------------------
// 控件 ID
// ---------------------------------------------------------------------------

const ID_PICK: usize = 100;
const ID_SAI_NAVIGATOR: usize = 101;
const ID_HELP: usize = 102;
const ID_STATUS: usize = 103;
const ID_HDR_REC: usize = 110;
const ID_LBL_FPS: usize = 111;
const ID_CMB_RECFPS: usize = 112;
const ID_LBL_THR: usize = 113;
const ID_CMB_SENS: usize = 114;
const ID_HINT_SENS: usize = 115;
const ID_BTN_CLEAN: usize = 116;
const ID_HDR_EXP: usize = 120;
const ID_CHK_CLEAN: usize = 122;
const ID_HDR_HIST: usize = 130;
const ID_LIST: usize = 131;
const ID_REEXPORT: usize = 132;
const ID_OPEN_DIR: usize = 133;
/// 【打开视频文件夹】（打开合成文件所在目录）
const ID_OPEN_OUT_DIR: usize = 135;
const ID_DELETE: usize = 134;
const ID_RECORD: usize = 140;
const ID_PROGRESS: usize = 142;
const ID_CANCEL: usize = 143;

const TIMER_UI: usize = 1;
/// 自定义“真正销毁窗口”消息。不能在 App 方法里同步 DestroyWindow：
/// WM_DESTROY 会立刻 drop 掉 GWLP_USERDATA 里的 App，而调用栈还持有 `&mut App`。
/// 先 Post 到消息队列，等当前方法返回后再销毁。
const WM_APP_DESTROY: u32 = 0x8001;
/// 界面实时预览的抓帧上限宽度：160×100 的框用 480 宽足够，
/// 比每帧拷整张画布省一两个数量级的带宽/分配。
const PREVIEW_MAX_W: i32 = 480;

const PREVIEW_W: i32 = 140;
const PREVIEW_H: i32 = 88;
const DEFAULT_W: i32 = 330;
const DEFAULT_H: i32 = 344;
const MIN_W: i32 = 320;
const MIN_H: i32 = 300;

/// 帧密度四档：(文案键, 每秒采样帧数) —— 文案键而不是字符串，因为语言在运行时才确定
const REC_FPS_KEYS: [(lang::K, f64); 4] = [
    (lang::K::Fps1of5, 0.2),
    (lang::K::Fps1of2, 0.5),
    (lang::K::Fps1, 1.0),
    (lang::K::Fps2, 2.0),
];
/// 帧密度默认第 1 档（1 帧 / 5 秒）
const REC_FPS_DEFAULT: usize = 0;

/// 画面检测灵敏度 8 档：档位 1~8 → 变化像素比例阈值（数字越大越灵敏）
const SENS_LEVELS: [f32; 8] = [0.016, 0.014, 0.012, 0.010, 0.008, 0.006, 0.002, 0.001];
/// 默认第 6 档（0.006）
const SENS_DEFAULT: usize = 5;

/// 播放帧率与画质固定，不提供选择
const EXPORT_FPS: f64 = 30.0;
const EXPORT_QUALITY: Quality = Quality::Medium;

/// 由 fps 反查档位文字（日志/标签用）
fn rec_fps_label(fps: f64) -> &'static str {
    REC_FPS_KEYS
        .iter()
        .find(|(_, v)| (v - fps).abs() < 1e-9)
        .map(|(k, _)| lang::s(*k))
        .unwrap_or("custom")
}

/// 由阈值反查灵敏度档位（1~8；非标准值取最近的档）
fn sens_level_of(threshold: f32) -> usize {
    let mut best = 0usize;
    let mut best_d = f32::MAX;
    for (i, v) in SENS_LEVELS.iter().enumerate() {
        let d = (*v - threshold).abs();
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    best + 1
}

fn cls(s: &str) -> Vec<u16> {
    win::wide(s)
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum PickState {
    Empty,
    Ok,
}

struct EncState {
    progress: Arc<AtomicU32>,
    total: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    outcome: Arc<Mutex<Option<Result<EncodeOutcome, String>>>>,
    out: PathBuf,
    session: PathBuf,
    /// 合成成功后是否清理该会话缓存。
    /// **重新导出时必须为 false** —— 实测踩过：重新导出的输出默认落在会话目录里，
    /// 清缓存会把刚导出的视频一起删掉，表现为"重新导出没产出文件"。
    clean_after: bool,
    /// 合成线程句柄。保留它才能在退出时 join，避免进程退出把正在写
    /// trailer 的编码线程直接杀掉。
    worker: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct Ctl {
    title: HWND,
    btn_pick: HWND,
    btn_sai_navigator: HWND,
    btn_help: HWND,
    status: HWND,
    hdr_rec: HWND,
    lbl_fps: HWND,
    cmb_recfps: HWND,
    lbl_thr: HWND,
    cmb_sens: HWND,
    hint_sens: HWND,
    hdr_exp: HWND,
    chk_clean: HWND,
    hdr_hist: HWND,
    list: HWND,
    btn_reexport: HWND,
    btn_open: HWND,
    btn_open_out: HWND,
    btn_delete: HWND,
    btn_record: HWND,
    progress: HWND,
    btn_cancel: HWND,
    btn_clean: HWND,
}

struct App {
    hwnd: HWND,
    ctl: Ctl,
    font: HGDIOBJ,
    rec_open: bool,
    exp_open: bool,
    hist_open: bool,
    fps: f64,
    threshold: f32,
    auto_clean: bool,
    target: Option<win::WindowInfo>,
    full_view_mode: bool,
    full_view_rect: Option<crate::navigator::CanvasRect>,
    full_view_tip_shown: bool,
    sai_navigator_mode: bool,
    preview: Option<Frame>,
    /// 完整画布预览的直接抓取原始缓冲；裁剪结果另存 `preview`，避免每帧重分配。
    preview_raw: Frame,
    preview_rect: RECT,
    pick_state: PickState,
    preview_cap: Option<ThumbnailCapture>,
    sessions: Vec<crate::session::SessionSummary>,
    sel: Option<usize>,
    shared: Option<Arc<Shared>>,
    worker: Option<JoinHandle<()>>,
    bar: HWND,
    enc: Option<EncState>,
    /// 用户点了关闭，正在等录制/合成线程收尾；收尾完成后由 on_timer 销毁窗口。
    closing: bool,
    closing_since: Option<std::time::Instant>,
    relayouting: bool,
    /// 预览抓帧失败的上一次记录：`(错误串, 时间)` —— 目标窗口关闭/最小化时
    /// 这个错误会每 500ms 重复一次，只在"错误变了或过了 10 秒"时写日志，避免刷屏。
    preview_err: Option<(String, std::time::Instant)>,
    /// 上一次成功抓到的预览尺寸，尺寸变化时记一条日志（换目标/改窗口大小时有用）
    preview_size: (i32, i32),
    /// 预览抓帧是否因"主窗口不可见"而暂停（对应的 DWM 缩略图已关掉合成）
    preview_hidden: bool,
    /// 预览抓帧耗时累计（每 20 次写一条 `[perf]`，用于验证降采样的效果）
    preview_us: u64,
    preview_grabs: u32,
}

/// 有界等待，但不消费 JoinHandle，超时后调用方仍可保留它。
fn wait_handle(handle: &JoinHandle<()>, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while !handle.is_finished() {
        if std::time::Instant::now() >= deadline {
            logline!("[exit] 线程超时未收尾，放弃等待");
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    true
}

/// 有界等待线程收尾。线程卡在 PrintWindow/编码里时，宁可退出也不能让 UI 无限挂住。
fn join_with_timeout(handle: Option<JoinHandle<()>>, timeout: std::time::Duration) -> bool {
    let Some(handle) = handle else {
        return true;
    };
    let finished = wait_handle(&handle, timeout);
    if finished {
        let _ = handle.join();
    }
    finished
}

impl App {
    fn status_text(&self) -> String {
        if self.sai_navigator_mode {
            let size = self
                .full_view_rect
                .map(|rect| format!("{}×{}", rect.width, rect.height))
                .unwrap_or_else(|| "SAI2".to_string());
            return lang::f(lang::K::StatusNavigator, &[&size]);
        }
        if self.full_view_mode {
            let size = self
                .full_view_rect
                .map(|rect| format!("{}×{}", rect.width, rect.height))
                .unwrap_or_else(|| "Navigator".to_string());
            return lang::f(lang::K::StatusFullView, &[&size]);
        }
        // 把目标尺寸/类名带出来：用户一眼就能分辨"选中的是画布还是整个窗口"
        // （之前只显示"已选中画布窗口"，选到主窗口也照样这么说，没法判断）。
        let picked = self
            .target
            .as_ref()
            .map(|t| {
                let (w, h) = t.client_size();
                format!("{w}×{h}")
            })
            .unwrap_or_default();
        match self.pick_state {
            PickState::Empty => lang::s(lang::K::StatusEmpty).to_string(),
            PickState::Ok => lang::f(lang::K::StatusOkPlain, &[&picked]),
        }
    }

    fn crop_navigator_preview(&self, frame: &Frame) -> Option<Frame> {
        if !self.full_view_mode {
            return None;
        }
        let (Some(rect), Some(cap)) = (self.full_view_rect, self.preview_cap.as_ref()) else {
            return None;
        };
        let (source_w, source_h) = win::client_size(cap.target());
        if source_w <= 0 || source_h <= 0 {
            return None;
        }
        let x0 = rect.x as i64 * frame.width as i64 / source_w as i64;
        let y0 = rect.y as i64 * frame.height as i64 / source_h as i64;
        let x1 = (rect.x as i64 + rect.width as i64) * frame.width as i64 / source_w as i64;
        let y1 = (rect.y as i64 + rect.height as i64) * frame.height as i64 / source_h as i64;
        frame
            .crop(x0 as i32, y0 as i32, (x1 - x0) as i32, (y1 - y0) as i32)
            .ok()
    }
}

// ---------------------------------------------------------------------------
// 布局
// ---------------------------------------------------------------------------

fn rect(x: i32, y: i32, w: i32, h: i32) -> RECT {
    RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}

/// 计算所有控件的目标位置，并返回（内容所需高度, 预览矩形）
///
/// 宽度按 330px 设计（用户要求）：左右边距 8px，控件按剩余宽度自适应。
fn compute_layout(app: &App, cw: i32) -> (Vec<(HWND, RECT, bool)>, RECT, i32) {
    let m = scale::px(8);
    let inner = (cw - m * 2).max(scale::px(120));
    let mut y = scale::px(6);
    let mut out: Vec<(HWND, RECT, bool)> = Vec::new();
    let c = &app.ctl;
    let mut push = |h: HWND, r: RECT, vis: bool| out.push((h, r, vis));

    push(c.title, rect(m, y, inner, scale::px(20)), true);
    y += scale::px(24);

    // 拾取模块按钮组（两个按钮均分）
    let gap = scale::px(6);
    let bw = (inner - gap) / 2;
    push(c.btn_pick, rect(m, y, bw, scale::px(24)), true);
    push(
        c.btn_help,
        rect(m + bw + gap, y, inner - bw - gap, scale::px(24)),
        true,
    );
    y += scale::px(30);
    push(c.btn_sai_navigator, rect(m, y, inner, scale::px(24)), true);
    y += scale::px(30);
    // 预览缩略图 + 右侧【开始录制】：按钮吃掉预览右侧的剩余宽度
    let preview_w = scale::px(PREVIEW_W);
    let preview_h = scale::px(PREVIEW_H);
    let prev = rect(m, y, preview_w, preview_h);
    let rec_w = (inner - preview_w - scale::px(12)).clamp(scale::px(90), scale::px(160));
    push(
        c.btn_record,
        rect(
            cw - m - rec_w,
            y + preview_h / 2 - scale::px(12),
            rec_w,
            scale::px(24),
        ),
        true,
    );
    y += preview_h + scale::px(6);

    push(c.status, rect(m, y, inner, scale::px(18)), true);
    y += scale::px(22);

    // 录制参数（不折叠，直接显示）。折叠标题栏控件保留但隐藏、不占高度。
    let label_w = scale::px(108);
    let sens_w = scale::px(56);
    push(c.hdr_rec, rect(m, y, inner, scale::px(22)), false);
    push(c.lbl_fps, rect(m, y, label_w, scale::px(22)), true);
    push(
        c.cmb_recfps,
        rect(m + label_w, y, inner - label_w, scale::px(22)),
        true,
    );
    y += scale::px(26);
    push(c.lbl_thr, rect(m, y, label_w, scale::px(22)), true);
    push(
        c.cmb_sens,
        rect(m + label_w, y, sens_w, scale::px(22)),
        true,
    );
    // 说明紧跟在选择框后面（不单独占一行）
    push(
        c.hint_sens,
        rect(
            m + label_w + sens_w + scale::px(6),
            y,
            inner - label_w - sens_w - scale::px(6),
            scale::px(22),
        ),
        true,
    );
    y += scale::px(26);

    // 导出视频参数（不折叠，直接显示）。播放帧率（固定 30fps）与画质（固定中）
    // 不提供选择；缓存提示改为最下方的按钮。
    push(c.hdr_exp, rect(m, y, inner, scale::px(22)), false);
    push(c.chk_clean, rect(m, y, inner, scale::px(20)), true);
    y += scale::px(22);
    // 输出位置不再让用户选（固定为 <exe 目录>\output），这一行整体去掉；
    // 想改就只能改程序目录结构 —— 少一个出错点。

    // 历史缓存会话：已移除整块（控件保留但永久隐藏，不占高度）
    push(c.hdr_hist, rect(m, y, inner, scale::px(22)), false);
    push(c.list, rect(m, y, inner, scale::px(96)), false);
    push(
        c.btn_reexport,
        rect(m, y, scale::px(70), scale::px(24)),
        false,
    );
    push(c.btn_open, rect(m, y, scale::px(70), scale::px(24)), false);
    push(
        c.btn_delete,
        rect(m, y, scale::px(70), scale::px(24)),
        false,
    );

    // 合成进度 + 取消合成
    let by = y.max(0) + scale::px(2);
    let cancel_w = scale::px(48);
    push(
        c.progress,
        rect(m, by, inner - scale::px(52), scale::px(18)),
        true,
    );
    push(
        c.btn_cancel,
        rect(cw - m - cancel_w, by, cancel_w, scale::px(20)),
        true,
    );
    // 最下方两个按钮：左【手动清理缓存】/ 右【打开视频文件夹】，各占一半
    let cy = by + scale::px(9);
    let gap = scale::px(6);
    let btn_w = (inner - gap) / 2;
    push(c.btn_clean, rect(m, cy, btn_w, scale::px(24)), true);
    push(
        c.btn_open_out,
        rect(m + btn_w + gap, cy, inner - btn_w - gap, scale::px(24)),
        true,
    );
    let need = cy + scale::px(40); // 按钮与下边框留 16px（用户两次各要求 +5px）

    (out, prev, need)
}

// ---------------------------------------------------------------------------
// 控件创建辅助
// ---------------------------------------------------------------------------

unsafe fn mk(
    parent: HWND,
    class: &str,
    text: &str,
    style: u32,
    id: usize,
    app_font: HGDIOBJ,
) -> HWND {
    let c = cls(class);
    let t = win::wide(text);
    let h = CreateWindowExW(
        0,
        c.as_ptr(),
        t.as_ptr(),
        style,
        0,
        0,
        10,
        10,
        parent,
        id as _,
        win::module_handle(),
        std::ptr::null(),
    );
    SendMessageW(h, crate::ui::WM_SETFONT_C, app_font as WPARAM, 1);
    h
}

// windows_sys 没把 WM_SETFONT 直接导出成我们想要的常量名时用它兜底
pub const WM_SETFONT_C: u32 = 0x0030;

// ---------------------------------------------------------------------------
// 窗口过程
// ---------------------------------------------------------------------------

unsafe extern "system" fn main_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let app = Box::new(App::new(hwnd));
            let ptr = Box::into_raw(app);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);
            (*ptr).build_controls();
            (*ptr).relayout(true);
            0
        }
        WM_SIZE => {
            if let Some(app) = app_mut(hwnd) {
                app.relayout(false);
            }
            0
        }
        // WM_DPICHANGED：PerMonitorV2 下窗口跨显示器时必须自己缩放。
        0x02E0 => {
            scale::set((wparam & 0xffff) as u32);
            let suggested = lparam as *const RECT;
            if !suggested.is_null() {
                let r = *suggested;
                let _ = SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | 0x0010, // SWP_NOACTIVATE
                );
            }
            if let Some(app) = app_mut(hwnd) {
                app.font = theme::font();
                app.apply_font();
                app.relayout(false);
            }
            0
        }
        WM_GETMINMAXINFO => {
            let mmi = lparam as *mut windows_sys::Win32::UI::WindowsAndMessaging::MINMAXINFO;
            (*mmi).ptMinTrackSize.x = scale::px(MIN_W);
            (*mmi).ptMinTrackSize.y = scale::px(MIN_H);
            0
        }
        // WM_CTLCOLORSTATIC = 0x0138：wparam 是 DC，lparam 是控件句柄。
        // 所有标签/状态文字都走深色底 + 浅色字；lang::s(lang::K::StartRecording)已改为自绘按钮，不再特判。
        0x0138 => theme::paint_static(wparam as _),
        // WM_CTLCOLOREDIT = 0x0133：输出路径输入框深色底 + 浅色字
        0x0133 => theme::paint_edit(wparam as _),
        WM_COMMAND => {
            let id = wparam & 0xffff;
            let code = ((wparam >> 16) & 0xffff) as u32;
            if let Some(app) = app_mut(hwnd) {
                app.on_command(id, code);
            }
            0
        }
        WM_HSCROLL => 0, // 已无滑块（帧密度/灵敏度改下拉），留空避免消息落进 DefWindowProc
        WM_TIMER => {
            if let Some(app) = app_mut(hwnd) {
                app.on_timer();
            }
            0
        }
        WM_PAINT => {
            if let Some(app) = app_mut(hwnd) {
                app.on_paint();
            }
            0
        }
        WM_CLOSE => {
            if let Some(app) = app_mut(hwnd) {
                app.request_close();
            } else {
                DestroyWindow(hwnd);
            }
            0
        }
        WM_APP_DESTROY => {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
            if !ptr.is_null() {
                // 先摘掉指针，WM_DESTROY 就不会再 drop 一次。
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(ptr));
            }
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
            if !ptr.is_null() {
                drop(Box::from_raw(ptr));
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 把 panic 内容转成一句话（`&str` / `String` / 其它）
fn panic_msg(e: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = e.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = e.downcast_ref::<String>() {
        s.clone()
    } else {
        "未知 panic（非字符串 payload）".to_string()
    }
}

/// 窗口过程的**兜底壳**：panic 一旦跨过 `extern "system"` 边界会直接 abort，
/// 用户看到的就是"程序突然没了"。这里接住 panic、写日志、返回 0，进程继续活着。
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        main_wndproc(hwnd, msg, wparam, lparam)
    })) {
        Ok(v) => v,
        Err(e) => {
            logline!("[panic] wndproc msg=0x{msg:X}: {}", panic_msg(&e));
            0
        }
    }
}

unsafe fn app_mut(hwnd: HWND) -> Option<&'static mut App> {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
    if ptr.is_null() {
        None
    } else {
        Some(&mut *ptr)
    }
}

impl App {
    fn new(hwnd: HWND) -> App {
        // 先按窗口所在显示器设置缩放，再创建字体；否则 WM_CREATE 阶段
        // 字体还是按 96 DPI 建出来的。
        scale::set(win::dpi_of(hwnd));
        let font = theme::font();
        App {
            hwnd,
            ctl: Ctl::default(),
            font,
            rec_open: true,
            exp_open: true, // 按要求：导出参数不再折叠，直接显示
            hist_open: false,
            fps: REC_FPS_KEYS[REC_FPS_DEFAULT].1,
            threshold: SENS_LEVELS[SENS_DEFAULT],
            auto_clean: true,
            // 默认导出到"程序所在目录\output"（不再放在缓存目录里）
            target: None,
            full_view_mode: false,
            full_view_rect: None,
            full_view_tip_shown: false,
            sai_navigator_mode: false,
            preview: None,
            preview_raw: Frame::new(0, 0, Vec::new()),
            preview_rect: rect(0, 0, 0, 0),
            pick_state: PickState::Empty,
            preview_cap: None,
            sessions: Vec::new(),
            sel: None,
            shared: None,
            worker: None,
            bar: std::ptr::null_mut(),
            enc: None,
            closing: false,
            closing_since: None,
            relayouting: false,
            preview_err: None,
            preview_size: (0, 0),
            preview_hidden: false,
            preview_us: 0,
            preview_grabs: 0,
        }
    }

    #[allow(clippy::field_reassign_with_default)]
    unsafe fn build_controls(&mut self) {
        let h = self.hwnd;
        let f = self.font;
        let ws = WS_CHILD | WS_VISIBLE;
        let mut c = Ctl::default();
        c.title = mk(
            h,
            "Static",
            &format!("{} · {}", app_title(), lang::s(lang::K::Subtitle)),
            ws,
            0,
            f,
        );
        c.btn_pick = mk(h, "Button", lang::s(lang::K::FullViewMode), ws, ID_PICK, f);
        c.btn_sai_navigator = mk(
            h,
            "Button",
            lang::s(lang::K::SaiNavigatorMode),
            ws,
            ID_SAI_NAVIGATOR,
            f,
        );
        c.btn_help = mk(h, "Button", lang::s(lang::K::HowToLocate), ws, ID_HELP, f);
        c.status = mk(h, "Static", "", ws, ID_STATUS, f);

        c.hdr_rec = mk(
            h,
            "Button",
            &format!("▼ {}", lang::s(lang::K::HdrRecording)),
            ws,
            ID_HDR_REC,
            f,
        );
        c.lbl_fps = mk(
            h,
            "Static",
            lang::s(lang::K::FrameDensity),
            ws,
            ID_LBL_FPS,
            f,
        );
        // 下拉用自绘控件（系统 ComboBox 在深色下会保持白底）
        c.cmb_recfps = dropdown::create(h, ID_CMB_RECFPS, 0, 0, 120, 22);
        let fps_labels: Vec<&str> = REC_FPS_KEYS.iter().map(|(k, _)| lang::s(*k)).collect();
        dropdown::set_items(c.cmb_recfps, &fps_labels);
        dropdown::set_sel(c.cmb_recfps, REC_FPS_DEFAULT);
        c.lbl_thr = mk(
            h,
            "Static",
            lang::s(lang::K::Sensitivity),
            ws,
            ID_LBL_THR,
            f,
        );
        c.cmb_sens = dropdown::create(h, ID_CMB_SENS, 0, 0, 56, 22);
        let sens_labels: Vec<String> = (1..=SENS_LEVELS.len()).map(|i| i.to_string()).collect();
        let sens_refs: Vec<&str> = sens_labels.iter().map(|s| s.as_str()).collect();
        dropdown::set_items(c.cmb_sens, &sens_refs);
        dropdown::set_sel(c.cmb_sens, SENS_DEFAULT);
        c.hint_sens = mk(h, "Static", lang::s(lang::K::SensHint), ws, ID_HINT_SENS, f);

        c.hdr_exp = mk(
            h,
            "Button",
            &format!("▶ {}", lang::s(lang::K::HdrExport)),
            ws,
            ID_HDR_EXP,
            f,
        );
        c.chk_clean = mk(
            h,
            "Button",
            lang::s(lang::K::AutoClean),
            ws | 0x0000_0003,
            ID_CHK_CLEAN,
            f,
        );
        SendMessageW(c.chk_clean, 0x00F1, 1, 0); // BM_SETCHECK = BST_CHECKED

        c.hdr_hist = mk(
            h,
            "Button",
            &format!("▶ {}", lang::s(lang::K::HdrHistory)),
            ws,
            ID_HDR_HIST,
            f,
        );
        c.list = mk(
            h,
            "ListBox",
            "",
            ws | 0x0001 | 0x0020_0000 | 0x0080_0000,
            ID_LIST,
            f,
        ); // LBS_NOTIFY|WS_VSCROLL|WS_BORDER
        c.btn_reexport = mk(h, "Button", lang::s(lang::K::Reexport), ws, ID_REEXPORT, f);
        c.btn_open = mk(
            h,
            "Button",
            lang::s(lang::K::OpenFolder),
            ws,
            ID_OPEN_DIR,
            f,
        );
        c.btn_open_out = mk(
            h,
            "Button",
            lang::s(lang::K::OpenVideoFolder),
            ws,
            ID_OPEN_OUT_DIR,
            f,
        );
        c.btn_delete = mk(h, "Button", lang::s(lang::K::Delete), ws, ID_DELETE, f);

        // 【开始录制】用 Static 而非常规按钮：带视觉主题的按钮会忽略 WM_CTLCOLORBTN，
        // 而静态控件认 WM_CTLCOLORSTATIC，才能可靠地画成绿底白字。
        // SS_CENTER(0x1) | SS_NOTIFY(0x100) | SS_CENTERIMAGE(0x200)
        c.btn_record = mk(
            h,
            "Static",
            lang::s(lang::K::StartRecording),
            ws | 0x0000_0001 | 0x0000_0100 | 0x0000_0200,
            ID_RECORD,
            f,
        );
        c.progress = mk(h, "msctls_progress32", "", ws, ID_PROGRESS, f);
        c.btn_cancel = mk(
            h,
            "Button",
            lang::s(lang::K::CancelExport),
            ws,
            ID_CANCEL,
            f,
        );
        c.btn_clean = mk(
            h,
            "Button",
            lang::s(lang::K::CleanCache),
            ws,
            ID_BTN_CLEAN,
            f,
        );
        SendMessageW(c.progress, PBM_SETRANGE32, 0, 100);

        // 深色主题：按钮/复选框/进度条自绘；下拉框与输入框交给系统深色主题
        theme::decorate(c.btn_pick, theme::Role::Ghost);
        theme::decorate(c.btn_sai_navigator, theme::Role::Ghost);
        theme::decorate(c.btn_help, theme::Role::Ghost);
        theme::decorate(c.btn_record, theme::Role::Accent);
        theme::decorate(c.btn_open_out, theme::Role::Ghost);
        theme::decorate(c.btn_cancel, theme::Role::Ghost);
        theme::decorate(c.btn_clean, theme::Role::Ghost);
        theme::decorate(c.chk_clean, theme::Role::Check);
        theme::decorate(c.progress, theme::Role::Progress);
        // 下拉是自绘的，不需要系统主题
        self.ctl = c;

        // 初始可见性：拾取参数展开、导出/历史折叠
        self.set_panel_visible();
        // 会话列表**懒加载**：历史面板默认折叠，启动时没必要扫几百个目录、几万个 PNG。
        // 展开面板（ID_HDR_HIST）时才真正读盘。
        self.refresh_labels();
        self.ui_set_idle();
    }

    unsafe fn set_panel_visible(&self) {
        let c = &self.ctl;
        let show = |h: HWND, v: bool| {
            ShowWindow(h, if v { SW_SHOW } else { 0 });
        };
        for h in [c.lbl_fps, c.cmb_recfps, c.lbl_thr, c.cmb_sens, c.hint_sens] {
            show(h, self.rec_open);
        }
        show(c.chk_clean, self.exp_open);
        for h in [c.list, c.btn_reexport, c.btn_open, c.btn_delete] {
            show(h, self.hist_open);
        }
    }

    unsafe fn refresh_labels(&self) {
        // 帧密度 / 灵敏度现在是下拉框：值就显示在框里，标签只写名字，不再重复显示数值
        let t = win::wide(&self.status_text());
        SetWindowTextW(self.ctl.status, t.as_ptr());
    }

    /// DPI 切换后，标准 Static/Button/ListBox 仍持有创建时的字体句柄，
    /// 需要显式重新 WM_SETFONT；自绘控件每次 paint 时取 theme::font()，不受影响。
    unsafe fn apply_font(&self) {
        let f = theme::font();
        let c = &self.ctl;
        for h in [
            c.title,
            c.btn_pick,
            c.btn_sai_navigator,
            c.btn_help,
            c.status,
            c.hdr_rec,
            c.lbl_fps,
            c.cmb_recfps,
            c.lbl_thr,
            c.cmb_sens,
            c.hint_sens,
            c.hdr_exp,
            c.chk_clean,
            c.hdr_hist,
            c.list,
            c.btn_reexport,
            c.btn_open,
            c.btn_open_out,
            c.btn_delete,
            c.btn_record,
            c.progress,
            c.btn_cancel,
            c.btn_clean,
        ] {
            if !h.is_null() {
                SendMessageW(h, WM_SETFONT_C, f as WPARAM, 1);
            }
        }
    }

    /// 下拉框的选择变化时，把档位写回 `fps` / `threshold`
    unsafe fn sync_from_combos(&mut self) {
        let c = &self.ctl;
        if !c.cmb_recfps.is_null() {
            let i = dropdown::get_sel(c.cmb_recfps);
            if let Some((_, v)) = REC_FPS_KEYS.get(i) {
                self.fps = *v;
            }
        }
        if !c.cmb_sens.is_null() {
            let i = dropdown::get_sel(c.cmb_sens);
            if let Some(v) = SENS_LEVELS.get(i) {
                self.threshold = *v;
            }
        }
        self.refresh_labels();
    }

    /// 重排控件；`initial` 时允许调整窗口尺寸
    unsafe fn relayout(&mut self, initial: bool) {
        if self.relayouting {
            return;
        }
        self.relayouting = true;
        dropdown::close_open();
        let mut cr: RECT = std::mem::zeroed();
        GetClientRect(self.hwnd, &mut cr);
        // 布局按**实际客户区**宽度算：MIN_W 是窗口最小宽度，不是客户区宽度，
        // 用 .max(MIN_W) 会把 330 宽的窗口按 320 客户区排，右边缘控件整体右移。
        let cw = (cr.right - cr.left).max(200);
        let (items, prev, need) = compute_layout(self, cw);
        for (h, r, vis) in items {
            if h.is_null() {
                continue;
            }
            MoveWindow(h, r.left, r.top, r.right - r.left, r.bottom - r.top, 1);
            if !vis {
                ShowWindow(h, 0);
            }
        }
        self.preview_rect = prev;
        // 【开始录制】的圆角改由 theme 自绘（不再用窗口区域裁剪，避免锯齿）
        self.set_panel_visible();

        // 内容装不下就把窗口长高（规格给了最小尺寸，说明窗口可变）
        let ch = cr.bottom - cr.top;
        if need > ch {
            let mut wr: RECT = std::mem::zeroed();
            windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect(self.hwnd, &mut wr);
            let frame_h = (wr.bottom - wr.top) - ch;
            let target = need + frame_h;
            let _ = SetWindowPos(
                self.hwnd,
                std::ptr::null_mut(),
                0,
                0,
                (wr.right - wr.left).max(scale::px(DEFAULT_W)),
                target,
                SWP_NOZORDER | 0x0002 | 0x0004, // SWP_NOZORDER | SWP_NOMOVE | SWP_NOACTIVATE
            );
        } else if initial {
            let _ = SetWindowPos(
                self.hwnd,
                std::ptr::null_mut(),
                0,
                0,
                scale::px(DEFAULT_W),
                scale::px(DEFAULT_H),
                SWP_NOZORDER | 0x0002 | 0x0004,
            );
        }
        InvalidateRect(self.hwnd, std::ptr::null(), 1);
        self.relayouting = false;
    }

    unsafe fn refresh_session_list(&mut self) {
        self.sessions = Session::list(&default_cache_root());
        let lb = self.ctl.list;
        SendMessageW(lb, 0x0184, 0, 0); // LB_RESETCONTENT
        for s in &self.sessions {
            let text = lang::f(lang::K::SessionItem, &[&s.name, &s.frames]);
            let w = win::wide(&text);
            SendMessageW(lb, 0x0180, 0, w.as_ptr() as isize); // LB_ADDSTRING
        }
        self.sel = None;
    }

    unsafe fn selected_session(&self) -> Option<&crate::session::SessionSummary> {
        let i = SendMessageW(self.ctl.list, 0x0188, 0, 0) as i32; // LB_GETCURSEL
        if i < 0 {
            None
        } else {
            self.sessions.get(i as usize)
        }
    }

    fn on_command(&mut self, id: usize, _code: u32) {
        unsafe {
            match id {
                ID_PICK => self.select_full_view(),
                ID_SAI_NAVIGATOR => self.select_sai_navigator(),
                ID_HELP => self.show_help(),
                ID_BTN_CLEAN => {
                    // 打开缓存根目录，让用户自己清理（替代原来的"提示"文字）
                    let root = default_cache_root();
                    let _ = std::fs::create_dir_all(&root);
                    open_folder(&root);
                }
                ID_HDR_REC => {
                    self.rec_open = !self.rec_open;
                    self.update_headers();
                    self.relayout(false);
                }
                ID_HDR_EXP => {
                    self.exp_open = !self.exp_open;
                    self.update_headers();
                    self.relayout(false);
                }
                ID_HDR_HIST => {
                    self.hist_open = !self.hist_open;
                    if self.hist_open {
                        self.refresh_session_list();
                    }
                    self.update_headers();
                    self.relayout(false);
                }
                ID_CHK_CLEAN => {
                    let v = SendMessageW(self.ctl.chk_clean, 0x00F0, 0, 0); // BM_GETCHECK
                    self.auto_clean = v == 1;
                }
                ID_CMB_RECFPS | ID_CMB_SENS => self.sync_from_combos(),
                ID_RECORD => {
                    if self.enc.is_some() {
                        message(
                            self.hwnd,
                            lang::s(lang::K::Notice),
                            lang::s(lang::K::ExportInProgress),
                        );
                        return;
                    }
                    let running = self
                        .shared
                        .as_ref()
                        .map(|s| s.phase().is_running())
                        .unwrap_or(false);
                    if running || self.worker.is_some() {
                        if let Some(s) = &self.shared {
                            s.request_stop();
                        }
                    } else {
                        self.start_recording();
                    }
                }
                ID_CANCEL => {
                    if let Some(e) = &self.enc {
                        e.cancel.store(true, Ordering::SeqCst);
                    }
                }
                ID_REEXPORT => self.reexport_selected(),
                ID_OPEN_OUT_DIR => {
                    let dir = crate::default_output_dir();
                    open_folder(&dir);
                }
                ID_OPEN_DIR => {
                    if let Some(s) = self.selected_session() {
                        let p = s.dir.clone();
                        open_folder(&p);
                    }
                }
                ID_DELETE => self.delete_selected(),
                _ => {}
            }
        }
    }

    unsafe fn update_headers(&self) {
        let set = |h: HWND, open: bool, text: &str| {
            let t = win::wide(&format!("{} {}", if open { "▼" } else { "▶" }, text));
            SetWindowTextW(h, t.as_ptr());
        };
        set(
            self.ctl.hdr_rec,
            self.rec_open,
            lang::s(lang::K::HdrRecording),
        );
        set(self.ctl.hdr_exp, self.exp_open, lang::s(lang::K::HdrExport));
        set(
            self.ctl.hdr_hist,
            self.hist_open,
            lang::s(lang::K::HdrHistory),
        );
    }

    unsafe fn select_full_view(&mut self) {
        if self.enc.is_some()
            || self.worker.is_some()
            || self.shared.as_ref().is_some_and(|s| s.phase().is_running())
        {
            message(
                self.hwnd,
                lang::s(lang::K::Notice),
                lang::s(lang::K::ExportInProgress),
            );
            return;
        }
        if !self.full_view_tip_shown {
            message(
                self.hwnd,
                lang::s(lang::K::FullViewTitle),
                lang::s(lang::K::FullViewHelp),
            );
            self.full_view_tip_shown = true;
        }
        // 拾取期间：**始终置顶 + 半透明**（按用户要求）。
        // 置顶保证提示始终可见；半透明让我们不完全挡住下面的目标，方便点它的标题栏。
        // GWL_EXSTYLE = -20，WS_EX_LAYERED = 0x00080000，LWA_ALPHA = 0x2
        let ex = GetWindowLongPtrW(self.hwnd, -20) as i64;
        // 按用户要求：**不再半透明、不再鼠标穿透**，改为拾取时最小化。
        // 这里只把扩展样式保持原样（清掉 LAYERED/TRANSPARENT 位以防残留）。
        SetWindowLongPtrW(self.hwnd, -20, (ex & !(0x0008_0000 | 0x0000_0020)) as isize);
        // HWND_TOPMOST = -1；SWP_NOMOVE|NOSIZE|NOACTIVATE
        SetWindowPos(
            self.hwnd,
            (-1isize) as HWND, // HWND_TOPMOST
            0,
            0,
            0,
            0,
            0x0002 | 0x0001 | 0x0010,
        );
        // 拾取期间最小化主窗口（用户要求），让开目标；拾取结束再恢复。
        ShowWindow(self.hwnd, SW_MINIMIZE);
        logline!("[pick] 主窗口已最小化，进入拾取");
        // 说明清楚：拾取时 CanLapse 已最小化到任务栏（点不到），唯一该点的是画布边框。
        let t = win::wide(lang::s(lang::K::PickInstr));
        SetWindowTextW(self.ctl.status, t.as_ptr());
        UpdateWindow(self.hwnd);

        let hint = win::PickHintGuard(win::pick_hint_create(lang::s(lang::K::PickHint)));
        let picked = win::pick_window_interactive(self.hwnd, Some(hint.0));

        // 拾取结束后继续保持置顶，避免 SAI2/CSP 把主界面压到后面。
        SetWindowPos(
            self.hwnd,
            (-1isize) as HWND, // HWND_TOPMOST
            0,
            0,
            0,
            0,
            0x0002 | 0x0001 | 0x0010, // SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE
        );
        let ex2 = GetWindowLongPtrW(self.hwnd, -20) as i64;
        // 去掉 LAYERED 与 TRANSPARENT，恢复正常可点击
        SetWindowLongPtrW(
            self.hwnd,
            -20,
            (ex2 & !(0x0008_0000 | 0x0000_0020)) as isize,
        );
        ShowWindow(self.hwnd, SW_RESTORE);
        SetForegroundWindow(self.hwnd);
        InvalidateRect(self.hwnd, std::ptr::null(), 1);

        let Some(hwnd) = picked else {
            logline!("[pick] 拾取取消（Esc / 主窗口被恢复 / 点在本程序窗口上）");
            self.refresh_labels();
            let t = win::wide(lang::s(lang::K::PickCanceled));
            SetWindowTextW(self.ctl.status, t.as_ptr());
            return;
        };
        let info = win::describe(hwnd);
        if info.process.eq_ignore_ascii_case("sai2.exe")
            || info.process.eq_ignore_ascii_case("sai.exe")
        {
            message(
                self.hwnd,
                lang::s(lang::K::CannotRecord),
                "SAI2 第二视图捕获不稳定，请使用【使用 SAI2 导航器】。",
            );
            return;
        }
        let (cw, ch) = info.client_size();
        logline!(
            "[full-view] 目标 0x{:X} | {} | 客户区 {}x{} | root=0x{:X} | 层级：{}",
            hwnd as isize,
            info.label(),
            cw,
            ch,
            win::root_of(hwnd) as isize,
            win::ancestor_chain(hwnd)
        );
        let full = match ThumbnailCapture::new_direct(hwnd).and_then(|mut cap| cap.grab()) {
            Ok(frame) => frame,
            Err(e) => {
                logline!("[full-view] 抓帧失败：{e}");
                let t = win::wide(&lang::f(lang::K::GrabFailed, &[&e]));
                SetWindowTextW(self.ctl.status, t.as_ptr());
                return;
            }
        };
        let rect = match crate::navigator::detect_canvas_rect(&full) {
            Ok(rect) => rect,
            Err(e) => {
                logline!("[full-view] 画布定位失败：{e}");
                message(
                    self.hwnd,
                    lang::s(lang::K::CannotRecord),
                    &lang::f(lang::K::NavigatorFailed, &[&e]),
                );
                return;
            }
        };
        let preview = match crate::navigator::crop_canvas(&full, rect) {
            Ok(frame) => frame,
            Err(e) => {
                message(
                    self.hwnd,
                    lang::s(lang::K::CannotRecord),
                    &lang::f(lang::K::NavigatorFailed, &[&e]),
                );
                return;
            }
        };
        self.preview_cap = None;
        self.preview_cap = ThumbnailCapture::new_scaled_direct(hwnd, scale::px(PREVIEW_MAX_W)).ok();
        self.preview_size = (preview.width, preview.height);
        self.preview_raw = full;
        self.preview = Some(preview);
        self.preview_err = None;
        self.full_view_mode = true;
        self.full_view_rect = Some(rect);
        self.sai_navigator_mode = false;
        self.pick_state = PickState::Ok;
        logline!(
            "[pick] 拾取完成 pick_state={:?} 预览={}x{}",
            self.pick_state,
            self.preview.as_ref().map(|f| f.width).unwrap_or(0),
            self.preview.as_ref().map(|f| f.height).unwrap_or(0)
        );
        self.target = Some(info);
        self.refresh_labels();
        // 拾取期间主窗口被最小化过，WM_PAINT 的时机不保证；这里**同步**重绘一次，
        // 保证"点完标题栏，预览立刻出现"，而不是等下一个计时器/曝光周期。
        InvalidateRect(self.hwnd, std::ptr::null(), 1);
        UpdateWindow(self.hwnd);
        logline!("[pick] 同步重绘完成");
    }

    unsafe fn select_sai_navigator(&mut self) {
        if self.enc.is_some()
            || self.worker.is_some()
            || self.shared.as_ref().is_some_and(|s| s.phase().is_running())
        {
            message(
                self.hwnd,
                lang::s(lang::K::Notice),
                lang::s(lang::K::ExportInProgress),
            );
            return;
        }
        let selected = self
            .target
            .as_ref()
            .filter(|target| {
                target.process.eq_ignore_ascii_case("sai2.exe")
                    || target.process.eq_ignore_ascii_case("sai.exe")
            })
            .map(|target| target.hwnd)
            .or_else(|| crate::navigator::find_sai2_main().map(|window| window.hwnd));
        let Some(selected) = selected else {
            message(
                self.hwnd,
                lang::s(lang::K::CannotRecord),
                lang::s(lang::K::SaiNavigatorMissing),
            );
            return;
        };
        let nav = match crate::navigator::probe(selected) {
            Ok(nav) => nav,
            Err(e) => {
                let detail = if e == lang::s(lang::K::SaiNavigatorMissing) {
                    e
                } else {
                    lang::f(lang::K::NavigatorFailed, &[&e])
                };
                message(self.hwnd, lang::s(lang::K::CannotRecord), &detail);
                return;
            }
        };
        let full =
            match ThumbnailCapture::new_direct(nav.capture.hwnd).and_then(|mut cap| cap.grab()) {
                Ok(frame) => frame,
                Err(e) => {
                    message(
                        self.hwnd,
                        lang::s(lang::K::CannotRecord),
                        &lang::f(lang::K::NavigatorFailed, &[&e]),
                    );
                    return;
                }
            };
        let rect = match crate::navigator::detect_canvas_rect(&full) {
            Ok(rect) => rect,
            Err(e) => {
                message(
                    self.hwnd,
                    lang::s(lang::K::CannotRecord),
                    &lang::f(lang::K::NavigatorFailed, &[&e]),
                );
                return;
            }
        };
        let preview = match crate::navigator::crop_canvas(&full, rect) {
            Ok(frame) => frame,
            Err(e) => {
                message(
                    self.hwnd,
                    lang::s(lang::K::CannotRecord),
                    &lang::f(lang::K::NavigatorFailed, &[&e]),
                );
                return;
            }
        };
        self.preview_cap = None;
        self.preview_cap =
            ThumbnailCapture::new_scaled_direct(nav.capture.hwnd, scale::px(PREVIEW_MAX_W)).ok();
        self.preview_size = (preview.width, preview.height);
        self.preview_raw = full;
        self.preview = Some(preview);
        self.preview_err = None;
        self.target = Some(nav.capture.clone());
        self.full_view_mode = true;
        self.full_view_rect = Some(rect);
        self.sai_navigator_mode = true;
        self.pick_state = PickState::Ok;
        logline!(
            "[sai-navigator] root=0x{:X} capture=0x{:X} crop=({},{},{},{})",
            nav.root.hwnd as isize,
            nav.capture.hwnd as isize,
            rect.x,
            rect.y,
            rect.width,
            rect.height
        );
        self.refresh_labels();
        InvalidateRect(self.hwnd, std::ptr::null(), 1);
        UpdateWindow(self.hwnd);
    }

    unsafe fn show_help(&self) {
        message(
            self.hwnd,
            lang::s(lang::K::HelpTitle),
            lang::s(lang::K::HelpBody),
        );
    }

    unsafe fn start_recording(&mut self) {
        if self.enc.is_some() {
            message(
                self.hwnd,
                lang::s(lang::K::Notice),
                lang::s(lang::K::ExportInProgress),
            );
            return;
        }
        if self.worker.is_some() {
            if let Some(shared) = &self.shared {
                shared.request_stop();
            }
            return;
        }
        let Some(target) = &self.target else {
            message(
                self.hwnd,
                lang::s(lang::K::Notice),
                lang::s(lang::K::PickFirst),
            );
            return;
        };
        if !self.full_view_mode && !crate::capture::composition_enabled() {
            message(
                self.hwnd,
                lang::s(lang::K::CannotRecord),
                lang::s(lang::K::NoDwm),
            );
            return;
        }
        self.preview_cap = None; // 录制期间由录制线程独占抓帧
        logline!(
            "[record] 开始录制 target=0x{:X} | {} | fps={}（{}） | 灵敏度={}档 | threshold={:.3}",
            target.hwnd as isize,
            target.label(),
            self.fps,
            rec_fps_label(self.fps),
            sens_level_of(self.threshold),
            self.threshold
        );
        let cfg = RecorderConfig {
            hwnd: target.hwnd as isize,
            root: default_cache_root(),
            name: if target.process.is_empty() {
                lang::s(lang::K::CanvasFallback).to_string()
            } else {
                target.process.clone()
            },
            fps: self.fps,
            threshold: self.threshold,
            fill_rgb: [0, 0, 0],
            // GUI 流程启用"其他软件在前台就暂停"（用户点完录制会点进画布）
            fg_pause: true,
            full_view: self.full_view_mode && !self.sai_navigator_mode,
            navigator: self.sai_navigator_mode,
        };
        // 消耗掉录制开始前可能按住的 Esc，避免刚点开始就被误判为停止。
        let _ = win::escape_pressed_once();
        let shared = Shared::new();
        let worker = recorder::spawn(cfg, shared.clone());
        self.shared = Some(shared.clone());
        self.worker = Some(worker);
        let bar = floatbar::create(shared.clone(), Some(target.hwnd));
        let Some(bar) = bar else {
            // 没有悬浮条就没有暂停/标记/停止入口；GUI 又收不到主窗键盘消息。
            // 与其把主窗最小化后让用户无法停止，不如立刻取消这次录制并说明原因。
            shared.request_stop();
            if let Some(w) = self.worker.take() {
                let _ = w.join();
            }
            if shared.frames() == 0 {
                if let Some(d) = shared.session_dir() {
                    let _ = Session::delete_in(&default_cache_root(), &d);
                }
            }
            self.shared = None;
            if let Some(t) = &self.target {
                self.preview_cap = if self.full_view_mode {
                    ThumbnailCapture::new_scaled_direct(t.hwnd, scale::px(PREVIEW_MAX_W)).ok()
                } else {
                    ThumbnailCapture::new_scaled(t.hwnd, scale::px(PREVIEW_MAX_W)).ok()
                };
            }
            self.ui_set_idle();
            message(
                self.hwnd,
                lang::s(lang::K::RecordingFailed),
                lang::s(lang::K::BarFailed),
            );
            return;
        };
        self.bar = bar;
        // 规格：开始录制后主窗口自动最小化，只留悬浮条
        self.ui_set_recording();
        ShowWindow(self.hwnd, SW_MINIMIZE);
        SetTimer(self.hwnd, TIMER_UI, 500, None);
    }

    unsafe fn stop_and_encode(&mut self) {
        if let Some(w) = self.worker.take() {
            if !wait_handle(&w, std::time::Duration::from_secs(5)) {
                self.worker = Some(w);
                ShowWindow(self.hwnd, SW_RESTORE);
                SetForegroundWindow(self.hwnd);
                let text = win::wide(lang::s(lang::K::ThreadTimeout));
                SetWindowTextW(self.ctl.status, text.as_ptr());
                EnableWindow(self.ctl.btn_record, 0);
                EnableWindow(self.ctl.btn_pick, 0);
                EnableWindow(self.ctl.btn_sai_navigator, 0);
                message(
                    self.hwnd,
                    lang::s(lang::K::RecordingFailed),
                    lang::s(lang::K::ThreadTimeout),
                );
                return;
            }
            let _ = w.join();
        }
        let shared = self.shared.take();
        let dir = shared.as_ref().and_then(|s| s.session_dir());
        if !self.bar.is_null() {
            floatbar::destroy(self.bar);
            self.bar = std::ptr::null_mut();
        }
        // 规格：停止录制后主窗口自动恢复
        ShowWindow(self.hwnd, SW_RESTORE);
        SetForegroundWindow(self.hwnd);
        // 恢复实时预览（录制期间抓帧归录制线程独占）
        if let Some(t) = &self.target {
            self.preview_cap = if self.full_view_mode {
                ThumbnailCapture::new_scaled_direct(t.hwnd, scale::px(PREVIEW_MAX_W)).ok()
            } else {
                ThumbnailCapture::new_scaled(t.hwnd, scale::px(PREVIEW_MAX_W)).ok()
            };
        }

        let failed = shared.as_ref().and_then(|s| s.error());
        let frames = shared.as_ref().map(|s| s.frames()).unwrap_or(0);
        logline!(
            "[record] 停止：dir={:?} frames={} error={:?}",
            dir,
            frames,
            failed
        );
        // ⚠️ 每个提前返回的分支都要把 UI 恢复成待机态，否则会停在"录制中"
        // （【开始录制】藏起来、状态栏还写着"录制中…"）—— 实测踩过。
        if let Some(e) = failed {
            if frames == 0 {
                if let Some(d) = &dir {
                    if let Err(del_err) = Session::delete_in(&default_cache_root(), d) {
                        logline!("[cache] 清理空会话失败：{del_err}");
                    }
                }
            }
            self.ui_set_idle();
            message(self.hwnd, lang::s(lang::K::RecordingFailed), &e);
            return;
        }
        let Some(dir) = dir else {
            self.ui_set_idle();
            return;
        };
        if frames == 0 {
            // 需求/README 都是“没有帧就把会话丢掉”，不能留下 0 帧空目录。
            if let Err(del_err) = Session::delete_in(&default_cache_root(), &dir) {
                logline!("[cache] 清理空会话失败：{del_err}");
            }
            self.ui_set_idle();
            message(
                self.hwnd,
                lang::s(lang::K::RecordingEnded),
                lang::s(lang::K::NoFrames),
            );
            return;
        }
        // 规格：停止录制 → 进入视频合成流程（这条路径才适用"合成成功自动清理缓存"）
        // 输出文件名带本地时间戳：同名不会互相覆盖（v1.3 起）
        let out = export_out_path();
        let clean = self.auto_clean;
        self.start_encoding(dir, out, clean);
    }

    unsafe fn start_encoding(&mut self, session: PathBuf, out: PathBuf, clean_after: bool) {
        if self.enc.is_some() {
            logline!("[encode] 已有合成任务，忽略新的请求");
            return;
        }
        let progress = Arc::new(AtomicU32::new(0));
        let total = Arc::new(AtomicU32::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let outcome = Arc::new(Mutex::new(None));
        let st = EncState {
            progress: progress.clone(),
            total: total.clone(),
            cancel: cancel.clone(),
            finished: finished.clone(),
            outcome: outcome.clone(),
            out: out.clone(),
            session: session.clone(),
            clean_after,
            worker: None,
        };
        self.enc = Some(st);
        let opts = EncodeOptions {
            fps: EXPORT_FPS,
            quality: EXPORT_QUALITY,
            output: out,
        };
        logline!(
            "[encode] 开始 session={:?} out={:?} fps={} quality={:?} clean_after={}",
            session,
            opts.output,
            opts.fps,
            opts.quality,
            clean_after
        );
        let dir = session;
        let handle = std::thread::spawn(move || {
            // 合成是纯后台工作，降一档优先级，避免长时间导出抢占绘画操作。
            unsafe {
                windows_sys::Win32::System::Threading::SetThreadPriority(
                    windows_sys::Win32::System::Threading::GetCurrentThread(),
                    -1, // THREAD_PRIORITY_BELOW_NORMAL
                );
            }
            let started = std::time::Instant::now();
            // 合成线程 panic 时 UI 会永远停在"合成中"，这里接住并转成错误
            let r = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                encode::encode_session(&dir, &opts, &mut |done, all| {
                    progress.store(done, Ordering::SeqCst);
                    total.store(all, Ordering::SeqCst);
                    if cancel.load(Ordering::SeqCst) {
                        Progress::Cancel
                    } else {
                        Progress::Continue
                    }
                })
            })) {
                Ok(v) => v,
                Err(e) => Err(format!("合成线程 panic：{}", panic_msg(&e))),
            };
            crate::log::line(&format!(
                "[encode] 结束（{:.1}s）: {:?}",
                started.elapsed().as_secs_f64(),
                r.as_ref().map(|o| match o {
                    EncodeOutcome::Done(s) => format!(
                        "Done {}帧 {}x{} {:.1}s {}字节",
                        s.frames, s.width, s.height, s.seconds, s.bytes
                    ),
                    EncodeOutcome::Cancelled(s) =>
                        format!("Cancelled {}帧 {}x{}", s.frames, s.width, s.height),
                })
            ));
            if let Ok(mut slot) = outcome.lock() {
                *slot = Some(r);
            }
            finished.store(true, Ordering::SeqCst);
        });
        if let Some(enc) = &mut self.enc {
            enc.worker = Some(handle);
        }
        let t = win::wide(lang::s(lang::K::Preparing));
        SetWindowTextW(self.ctl.status, t.as_ptr());
        self.ui_set_encoding();
        SetTimer(self.hwnd, TIMER_UI, 500, None);
    }

    unsafe fn reexport_selected(&mut self) {
        let Some(s) = self.selected_session() else {
            message(
                self.hwnd,
                lang::s(lang::K::Notice),
                lang::s(lang::K::SelectFirst),
            );
            return;
        };
        let dir = s.dir.clone();
        // 输出放在会话目录**外面**，文件名包含会话目录名和帧率。会话目录本身
        // 保证唯一，因此不同会话不会互相覆盖；同一会话重复导出时再追加序号。
        let stem = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| s.name.clone());
        let out_parent = dir.parent().unwrap_or(&dir);
        let mut out = out_parent.join(format!("{stem}_{}fps.mp4", EXPORT_FPS as i32));
        let mut suffix = 2;
        while out.exists() {
            out = out_parent.join(format!("{stem}_{}fps_{suffix}.mp4", EXPORT_FPS as i32));
            suffix += 1;
        }
        // 重新导出面向的是"已有缓存"，不应该清理它
        self.start_encoding(dir, out, false);
    }

    unsafe fn delete_selected(&mut self) {
        let Some(s) = self.selected_session() else {
            message(
                self.hwnd,
                lang::s(lang::K::Notice),
                lang::s(lang::K::SelectFirst),
            );
            return;
        };
        let dir = s.dir.clone();
        let name = s.name.clone();
        let busy = self
            .shared
            .as_ref()
            .map(|sh| sh.phase().is_running())
            .unwrap_or(false)
            || self
                .enc
                .as_ref()
                .map(|e| !e.finished.load(Ordering::SeqCst))
                .unwrap_or(false);
        if busy {
            message(
                self.hwnd,
                lang::s(lang::K::CannotDelete),
                lang::s(lang::K::BusyDelete),
            );
            return;
        }
        // 规格：删除会话会删除整个会话缓存目录
        if !confirm(self.hwnd, &lang::f(lang::K::DeleteConfirm, &[&name])) {
            return;
        }
        match Session::delete_in(&default_cache_root(), &dir) {
            Ok(_) => {
                self.refresh_session_list();
                self.relayout(false);
            }
            Err(e) => message(self.hwnd, lang::s(lang::K::DeleteFailed), &e),
        }
    }

    /// 处理主窗口的 WM_CLOSE。
    ///
    /// 录制/合成时不能直接 DestroyWindow：JoinHandle 被 drop 只是 detach，
    /// 随后 PostQuitMessage 会让进程退出，正在写最后一帧或 MP4 trailer 的线程
    /// 会被直接杀掉。这里只请求停止并隐藏窗口，等 on_timer 看到线程收尾后再销毁。
    unsafe fn request_close(&mut self) {
        if self.closing {
            return;
        }
        let recording = self
            .shared
            .as_ref()
            .map(|s| s.phase().is_running())
            .unwrap_or(false)
            || self.worker.is_some();
        let encoding = self.enc.is_some();
        if !recording && !encoding {
            windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
                self.hwnd,
                WM_APP_DESTROY,
                0,
                0,
            );
            return;
        }
        self.closing = true;
        self.closing_since = Some(std::time::Instant::now());
        if let Some(shared) = &self.shared {
            shared.request_stop();
        }
        if let Some(enc) = &self.enc {
            enc.cancel.store(true, Ordering::SeqCst);
        }
        // 主窗在录制时本来就是最小化的；隐藏比恢复更能表达“正在退出”。
        ShowWindow(self.hwnd, 0);
        logline!("[exit] 收到关闭请求，等待录制/合成线程收尾");
    }

    /// 线程都收尾后再销毁窗口。调用方是 on_timer，仍在 UI 线程。
    unsafe fn finalize_close(&mut self) {
        if let Some(w) = self.worker.take() {
            join_with_timeout(Some(w), std::time::Duration::from_secs(3));
        }
        if let Some(mut enc) = self.enc.take() {
            enc.cancel.store(true, Ordering::SeqCst);
            if let Some(w) = enc.worker.take() {
                join_with_timeout(Some(w), std::time::Duration::from_secs(3));
            }
        }
        if !self.bar.is_null() {
            floatbar::destroy(self.bar);
            self.bar = std::ptr::null_mut();
        }
        windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(self.hwnd, WM_APP_DESTROY, 0, 0);
    }

    unsafe fn on_timer(&mut self) {
        if self.closing {
            let recording_done = self
                .shared
                .as_ref()
                .map(|s| matches!(s.phase(), Phase::Finished | Phase::Failed | Phase::Idle))
                .unwrap_or(true);
            let encoding_done = self
                .enc
                .as_ref()
                .map(|e| e.finished.load(Ordering::SeqCst))
                .unwrap_or(true);
            let expired = self
                .closing_since
                .map(|t| t.elapsed() >= std::time::Duration::from_secs(3))
                .unwrap_or(false);
            if (recording_done && encoding_done) || expired {
                if expired && !(recording_done && encoding_done) {
                    logline!("[exit] 关闭等待超时，强制结束未收尾线程");
                }
                self.finalize_close();
            }
            return;
        }
        // 录制状态
        if let Some(shared) = &self.shared {
            // 主窗在录制时是最小化的，收不到 WM_KEYDOWN；使用说明承诺的
            // “Esc 也能停止”只能在定时器里边沿轮询。
            if shared.phase().is_running() && !shared.stop_requested() && win::escape_pressed_once()
            {
                logline!("[record] 收到 Esc，请求停止");
                shared.request_stop();
            }
            // 用户一点"停止"就立刻把窗口叫回来，不等录制线程收尾、更不等合成
            if shared.phase().is_running() && shared.stop_requested() {
                ShowWindow(self.hwnd, SW_RESTORE);
                SetForegroundWindow(self.hwnd);
            }
            match shared.phase() {
                Phase::Finished | Phase::Failed => {
                    self.stop_and_encode();
                    return;
                }
                _ => {}
            }
        }
        // 合成进度
        if let Some(e) = &self.enc {
            let done = e.progress.load(Ordering::SeqCst);
            let total = e.total.load(Ordering::SeqCst);
            let pct = done
                .checked_mul(100)
                .and_then(|v| v.checked_div(total))
                .unwrap_or(0) as isize;
            SendMessageW(self.ctl.progress, PBM_SETPOS, pct as usize, 0);
            let t = win::wide(&lang::f(lang::K::ExportingStatus, &[&done, &total]));
            SetWindowTextW(self.ctl.status, t.as_ptr());
        }
        let enc_finished = self
            .enc
            .as_ref()
            .map(|e| e.finished.load(Ordering::SeqCst))
            .unwrap_or(false);
        if enc_finished {
            if let Some(mut enc) = self.enc.take() {
                // finished 在子线程最后一步设置；join 在这里只做确定性的收尾，
                // 同时避免关闭窗口时把还在写 trailer 的线程直接丢掉。
                if let Some(w) = enc.worker.take() {
                    let _ = w.join();
                }
                let outcome = enc.outcome.lock().ok().and_then(|mut o| o.take());
                let out = enc.out;
                let session = enc.session;
                let clean_after = enc.clean_after;
                SendMessageW(self.ctl.progress, PBM_SETPOS, 0, 0);
                self.ui_set_idle();
                match outcome {
                    Some(Ok(EncodeOutcome::Done(s))) => {
                        if clean_after {
                            // 规格：合成成功自动清理缓存（默认勾选）
                            if Session::delete_in(&default_cache_root(), &session).is_ok() {
                                self.refresh_session_list();
                            }
                        }
                        let mb = format!("{:.2}", s.bytes as f64 / 1048576.0);
                        let msg = lang::fill(
                            lang::s(lang::K::DlgDoneText),
                            &[&s.frames, &mb, &s.width, &s.height, &out.display()],
                        );
                        // 结果直接写进合成弹窗，1.6 秒后自动关闭（不再弹【确定】）
                        finish_enc_dialog(&msg);
                    }
                    Some(Ok(EncodeOutcome::Cancelled(s))) => {
                        finish_enc_dialog(&lang::f(
                            lang::K::DlgCanceledText,
                            &[&s.frames, &out.display()],
                        ));
                    }
                    Some(Err(err)) => finish_enc_dialog(&lang::f(lang::K::DlgFailedText, &[&err])),
                    None => {}
                }
            }
        }
        // 预览刷新（录制中不做，抓帧交给录制线程）
        let recording = self
            .shared
            .as_ref()
            .map(|s| s.phase().is_running())
            .unwrap_or(false);
        if !recording && self.enc.is_none() && self.preview_cap.is_some() {
            // 主窗口最小化 / 不可见时没人看预览：让 DWM 停掉缩略图合成并跳过抓帧。
            // 这是"后台占用"的大头 —— 缩略图是持续合成的，不看却一直在算。
            let hidden = unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::IsIconic(self.hwnd) != 0
                    || windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible(self.hwnd) == 0
            };
            if hidden {
                if !self.preview_hidden {
                    if let Some(cap) = &self.preview_cap {
                        let _ = cap.set_visible(false);
                    }
                    self.preview_hidden = true;
                    logline!("[preview] 主窗口不可见：暂停预览抓帧并关闭 DWM 合成");
                }
            } else {
                if self.preview_hidden {
                    if let Some(cap) = &self.preview_cap {
                        let _ = cap.set_visible(true);
                    }
                    self.preview_hidden = false;
                    self.preview_size = (0, 0); // 重新抓一帧，尺寸变化日志不作数
                    logline!("[preview] 主窗口重新可见：恢复预览抓帧");
                }
                // 先抓完再把结果搬出来，避免抓帧器借用挡住后面的字段修改。
                // 复用上一帧的 Frame，避免每 200/500ms 重分配预览缓冲。
                let full_view = self.full_view_mode;
                let mut refresh_crop = false;
                if let Some(cap) = self.preview_cap.as_mut() {
                    match cap.sync_source_size() {
                        Ok(changed) => refresh_crop = changed,
                        Err(e) => logline!("[preview] 同步源窗口尺寸失败：{e}"),
                    }
                }
                let mut frame = if full_view {
                    std::mem::replace(&mut self.preview_raw, Frame::new(0, 0, Vec::new()))
                } else {
                    self.preview.take().unwrap_or(Frame {
                        width: 0,
                        height: 0,
                        bgra: Vec::new(),
                    })
                };
                let t_grab = std::time::Instant::now();
                let grabbed = self.preview_cap.as_mut().map(|c| c.grab_into(&mut frame));
                self.preview_us += t_grab.elapsed().as_micros() as u64;
                self.preview_grabs += 1;
                if self.preview_grabs >= 20 {
                    let avg = self.preview_us as f64 / self.preview_grabs as f64 / 1000.0;
                    logline!(
                        "[perf] 预览抓帧 {} 次，平均 {avg:.2} ms/帧",
                        self.preview_grabs
                    );
                    self.preview_us = 0;
                    self.preview_grabs = 0;
                }
                if let Some(res) = grabbed {
                    match res {
                        Ok(()) => {
                            if full_view && refresh_crop {
                                if let Ok(rect) = crate::navigator::detect_canvas_rect(&frame) {
                                    self.full_view_rect = Some(rect);
                                    logline!(
                                        "[preview] 完整画布区域更新为 ({},{},{},{})",
                                        rect.x,
                                        rect.y,
                                        rect.width,
                                        rect.height
                                    );
                                }
                            }
                            let cropped = self.crop_navigator_preview(&frame);
                            let frame = if full_view {
                                if let Some(cropped) = cropped {
                                    self.preview_raw = frame;
                                    cropped
                                } else {
                                    self.preview_raw = frame.clone();
                                    frame
                                }
                            } else {
                                frame
                            };
                            let sz = (frame.width, frame.height);
                            if self.preview_size != (0, 0) && sz != self.preview_size {
                                logline!(
                                    "[preview] 输出尺寸变化 {}x{} -> {}x{}",
                                    self.preview_size.0,
                                    self.preview_size.1,
                                    sz.0,
                                    sz.1
                                );
                            }
                            if self.preview_err.take().is_some() {
                                logline!("[preview] 抓帧恢复正常");
                            }
                            self.preview_size = sz;
                            self.preview = Some(frame);
                            InvalidateRect(self.hwnd, &self.preview_rect, 0);
                        }
                        Err(e) => {
                            if full_view {
                                self.preview_raw = frame;
                            } else {
                                self.preview = Some(frame);
                            }
                            // 目标被关掉/最小化时这个错误每 500ms 重复一次：
                            // 只在"错误内容变了"或"距上次记录 ≥10 秒"时写
                            let now = std::time::Instant::now();
                            let due = match &self.preview_err {
                                None => true,
                                Some((old, at)) => {
                                    *old != e || now.duration_since(*at).as_secs() >= 10
                                }
                            };
                            if due {
                                logline!("[preview] 抓帧失败：{e}");
                                self.preview_err = Some((e, now));
                            }
                        }
                    }
                }
            }
        }
    }

    /// 底部操作区随状态切换（规格：Idle 显示【开始录制】；录制中只显示提示；
    /// 合成中整窗盖一层进度遮罩）。此前这三样一直可见，是明显的可用性问题。
    unsafe fn ui_set_idle(&mut self) {
        windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow(self.hwnd, 1);
        ShowWindow(self.ctl.btn_record, SW_SHOW);
        ShowWindow(self.ctl.progress, 0);
        ShowWindow(self.ctl.btn_cancel, 0);
        ShowWindow(self.ctl.btn_clean, SW_SHOW);
        // 恢复"已选中画布窗口…"这类常态信息（底部状态栏已移除，信息显示在这一行）
        self.refresh_labels();
        // 录制期间为了状态刷新把定时器调到了 200ms；空闲时恢复 500ms，
        // 否则预览抓帧会一直按 5fps 跑。
        SetTimer(self.hwnd, TIMER_UI, 500, None);
    }

    unsafe fn ui_set_recording(&self) {
        ShowWindow(self.ctl.btn_record, 0);
        ShowWindow(self.ctl.progress, 0);
        ShowWindow(self.ctl.btn_cancel, 0);
        let t = win::wide(lang::s(lang::K::RecordingHint));
        SetWindowTextW(self.ctl.status, t.as_ptr());
    }

    unsafe fn ui_set_encoding(&mut self) {
        // 规格（用户要求）：停止录制后**立即**恢复主窗口，并弹出独立的"合成进度"窗口；
        // 合成完成后弹窗自动消失（不再用 MessageBox 的【确定】）。
        ShowWindow(self.hwnd, SW_RESTORE);
        SetForegroundWindow(self.hwnd);
        // 合成期间主窗不接受操作；即使进度窗创建失败，也要由 on_timer
        // 在收到 outcome 后通过 ui_set_idle 重新启用。
        windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow(self.hwnd, 0);
        ShowWindow(self.ctl.btn_record, 0);
        if let Some(e) = &self.enc {
            show_enc_dialog(
                self.hwnd,
                e.progress.clone(),
                e.total.clone(),
                e.cancel.clone(),
            );
        }
    }

    unsafe fn on_paint(&mut self) {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let hdc = BeginPaint(self.hwnd, &mut ps);
        let r = self.preview_rect;
        let (bw, bh) = (r.right - r.left, r.bottom - r.top);
        // 预览图：把最近一帧等比缩放到 160×100 的框里
        let mut painted = false;
        if let Some(frame) = &self.preview {
            // 防御：缓冲区尺寸与声明尺寸不一致就别画（避免越界读）。
            // ⚠️ 这里**不能 return** —— 那会跳过 EndPaint，每异常一次漏一个 DC。
            let expect = (frame.width as usize).saturating_mul(frame.height as usize) * 4;
            let sane = frame.width > 0 && frame.height > 0 && frame.bgra.len() == expect;
            if sane && bw > 0 && bh > 0 {
                let scale = f64::min(
                    bw as f64 / frame.width as f64,
                    bh as f64 / frame.height as f64,
                );
                let dw = (frame.width as f64 * scale) as i32;
                let dh = (frame.height as f64 * scale) as i32;
                let dx = r.left + (bw - dw) / 2;
                let dy = r.top + (bh - dh) / 2;
                let bmi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: frame.width,
                        biHeight: -frame.height,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB,
                        biSizeImage: 0,
                        biXPelsPerMeter: 0,
                        biYPelsPerMeter: 0,
                        biClrUsed: 0,
                        biClrImportant: 0,
                    },
                    bmiColors: [RGBQUAD {
                        rgbBlue: 0,
                        rgbGreen: 0,
                        rgbRed: 0,
                        rgbReserved: 0,
                    }],
                };
                StretchDIBits(
                    hdc,
                    dx,
                    dy,
                    dw,
                    dh,
                    0,
                    0,
                    frame.width,
                    frame.height,
                    frame.bgra.as_ptr() as *const _,
                    &bmi,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
                painted = true;
            } else {
                logline!(
                    "[paint] 预览帧数据异常，跳过绘制（{}x{} len={}）",
                    frame.width,
                    frame.height,
                    frame.bgra.len()
                );
            }
        }
        if !painted && bw > 0 && bh > 0 {
            // 空框：画一圈浅凹陷边框 + 居中占位文字，让"这里会出画面"一眼可见。
            let edge = r;
            // 深色主题：用 1px 主题边框代替系统凹陷边
            {
                use windows_sys::Win32::Graphics::Gdi::FrameRect;
                let br = windows_sys::Win32::Graphics::Gdi::CreateSolidBrush(theme::BORDER);
                FrameRect(hdc, &edge, br);
                windows_sys::Win32::Graphics::Gdi::DeleteObject(br as _);
            }
            // ⚠️ 必须显式选 UI 字体。窗口 DC 的默认字体是 System 位图字体，
            //    高 DPI 下会大得离谱、又被 160×100 的框裁掉（实测踩过）。
            let old = SelectObject(hdc, self.font);
            SetBkMode(hdc, 1); // TRANSPARENT
            windows_sys::Win32::Graphics::Gdi::SetTextColor(hdc, theme::MUTED);
            let t = win::wide(lang::s(lang::K::PreviewEmpty));
            // 先量出两行文字的实际高度，再垂直居中 —— 固定 top 偏移在高 DPI 下会偏上。
            // ⚠️ DT_CALCRECT 会**改写矩形**（把 right/bottom 收到文字实际大小），
            //    所以必须用单独的 measure 矩形量，否则后面的居中会按被收窄的框算，
            //    文字表现为贴着左边（实测踩过）。
            let mut measure = RECT {
                left: r.left + 4,
                top: 0,
                right: r.right - 4,
                bottom: 0,
            };
            windows_sys::Win32::Graphics::Gdi::DrawTextW(
                hdc,
                t.as_ptr(),
                -1,
                &mut measure,
                0x0000_0400 | 0x0000_0001 | 0x0000_0010, // DT_CALCRECT | DT_CENTER | DT_WORDBREAK
            );
            let th = measure.bottom - measure.top;
            let mut rc = RECT {
                left: r.left + 4,
                top: r.top + (bh - th) / 2,
                right: r.right - 4,
                bottom: r.top + (bh - th) / 2 + th,
            };
            windows_sys::Win32::Graphics::Gdi::DrawTextW(
                hdc,
                t.as_ptr(),
                -1,
                &mut rc,
                0x0000_0001 | 0x0000_0010, // DT_CENTER | DT_WORDBREAK
            );
            SelectObject(hdc, old);
        }
        EndPaint(self.hwnd, &ps);
    }
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

// 目录策略统一在 lib.rs（<exe 目录>\cache / output，不可写时退回 LOCALAPPDATA）
/// exe 里的应用图标（IDI_ICON1）；没注入资源时返回 NULL，不影响运行。
///
/// # Safety
///
/// 调用方必须保证当前模块是已加载的主模块 —— `GetModuleHandleW` 返回的句柄
/// 指向本 exe。在本项目里只在 UI 初始化阶段调用一次，满足前提。
unsafe fn app_icon() -> *mut core::ffi::c_void {
    windows_sys::Win32::UI::WindowsAndMessaging::LoadImageW(
        win::module_handle(),
        101 as *const u16,
        1, // IMAGE_ICON
        0,
        0,
        0x0040 | 0x8000, // LR_DEFAULTSIZE | LR_SHARED
    )
}

fn default_cache_root() -> PathBuf {
    crate::default_cache_root()
}

fn default_output_dir() -> PathBuf {
    crate::default_output_dir()
}

/// 窗口标题 / 界面标题统一用这个名字（带版本号）
fn app_title() -> String {
    format!("CanLapse v{}", env!("CARGO_PKG_VERSION"))
}

/// 把已经在运行的 CanLapse 主窗口叫到前台（第二个实例只做这件事）
unsafe fn activate_existing() -> bool {
    let c = cls(MAIN_CLASS);
    // 先按类名找；个别系统上 FindWindow 的 title=NULL 不匹配，再退回按标题找
    let mut h =
        windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW(c.as_ptr(), std::ptr::null());
    if h.is_null() {
        let t = win::wide(&app_title());
        h = windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW(c.as_ptr(), t.as_ptr());
    }
    if h.is_null() {
        return false;
    }
    ShowWindow(h, SW_RESTORE);
    SetForegroundWindow(h);
    true
}

/// 默认导出路径：`<程序目录>\output\timelapse_YYYYMMDD-HHMMSS.mp4`
fn export_out_path() -> PathBuf {
    let stamp = win::stamp_compact(&win::local_now());
    let dir = default_output_dir();
    let mut path = dir.join(format!("timelapse_{stamp}.mp4"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("timelapse_{stamp}_{n}.mp4"));
        n += 1;
    }
    path
}

unsafe fn message(owner: HWND, title: &str, text: &str) {
    let t = win::wide(title);
    let b = win::wide(text);
    MessageBoxW(owner, b.as_ptr(), t.as_ptr(), MB_OK | MB_ICONINFORMATION);
}

unsafe fn confirm(owner: HWND, text: &str) -> bool {
    let t = win::wide(lang::s(lang::K::ConfirmTitle));
    let b = win::wide(text);
    // MB_YESNO | MB_ICONQUESTION
    windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
        owner,
        b.as_ptr(),
        t.as_ptr(),
        4 | 0x20,
    ) == 6
}

/// 用资源管理器打开文件夹（规格里的「打开文件夹」）
pub fn open_folder(dir: &std::path::Path) {
    let Some(exe) = win::windows_dir()
        .map(|d| d.join("explorer.exe"))
        .filter(|p| p.is_file())
    else {
        logline!(
            "[shell] 找不到系统 explorer.exe，无法打开 {}",
            dir.display()
        );
        return;
    };
    if let Err(e) = std::process::Command::new(exe).arg(dir).spawn() {
        logline!("[shell] 启动 explorer 失败：{e}");
    }
}

/// 启动 GUI（阻塞到窗口关闭）
pub fn run(initial_target: Option<isize>) -> i32 {
    // 运行日志：debug 构建默认写 %LOCALAPPDATA%\CanLapse\canlapse.log（GUI 没有控制台，
    // 用户测试后只能靠这个文件定位问题）；release 构建需显式设 CANLAPSE_LOG 才写。
    let _ = crate::log::init();
    let t_start = std::time::Instant::now();
    // 目录策略：缓存与成片都放在程序目录下；启动即建好，绿色分发时一眼可见
    let cache_root = crate::default_cache_root();
    let output_root = crate::default_output_dir();
    for dir in [&cache_root, &output_root] {
        let removed = crate::fsutil::cleanup_stale_temps(dir, crate::fsutil::STALE_TEMP_AGE);
        if removed > 0 {
            logline!(
                "[cleanup] 清理 {} 个过期临时文件：{}",
                removed,
                dir.display()
            );
        }
    }
    unsafe { theme::prefer_dark() };
    logline!("");
    logline!(
        "==== CanLapse {} ({}) 启动 ====",
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    logline!("exe = {:?}", std::env::current_exe().ok());
    logline!("日志 = {:?}", crate::log::sinks());
    logline!(
        "屏幕：虚拟桌面 {}x{} @({},{})，主屏 {}x{}",
        unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(78) }, // SM_CXVIRTUALSCREEN
        unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(79) }, // SM_CYVIRTUALSCREEN
        unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(76) }, // SM_XVIRTUALSCREEN
        unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(77) }, // SM_YVIRTUALSCREEN
        unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(0) },  // SM_CXSCREEN
        unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(1) },  // SM_CYSCREEN
    );
    // 单实例：第二个进程只负责把已有窗口叫到前台，然后退出。
    // 放这里（而不是 bin 入口）是因为 `CanLapse.exe` 和 `canlapse-cli gui` 都走 ui::run。
    if !win::single_instance_acquire() {
        logline!("[start] 已有实例在运行，激活它并退出");
        let ok = unsafe { activate_existing() };
        if !ok {
            unsafe {
                message(
                    std::ptr::null_mut(),
                    "CanLapse",
                    lang::s(lang::K::AlreadyRunning),
                );
            }
        }
        return 0;
    }
    // 崩溃现场落盘：GUI 是 windows 子系统，stderr 默认不可见，panic 信息会完全丢失。
    std::panic::set_hook(Box::new(|info| {
        let dir = std::env::var("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join("CanLapse");
        let _ = std::fs::create_dir_all(&dir);
        let stamp = win::stamp_iso(&win::local_now());
        let text = format!("[{stamp}] {info}\n");
        crate::log::line(&format!("[panic] {info}"));
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("crash.log"))
        {
            let _ = f.write_all(text.as_bytes());
        }
    }));
    // 未处理异常（访问冲突等）兜底：至少把异常码和地址落盘，方便事后定位。
    // 返回 CONTINUE_SEARCH —— 不吞异常，进程该退还是退，避免带病运行。
    unsafe {
        use windows_sys::Win32::System::Diagnostics::Debug::{
            SetUnhandledExceptionFilter, EXCEPTION_POINTERS,
        };
        unsafe extern "system" fn on_fatal(info: *const EXCEPTION_POINTERS) -> i32 {
            if !info.is_null() && !(*info).ExceptionRecord.is_null() {
                let rec = (*info).ExceptionRecord;
                let code = (*rec).ExceptionCode;
                let addr = (*rec).ExceptionAddress;
                // 只做固定缓冲写入，不分配、不解析路径。
                crate::log::crash_line(code as u32, addr as usize);
            }
            0 // EXCEPTION_CONTINUE_SEARCH
        }
        SetUnhandledExceptionFilter(Some(on_fatal));
    }
    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_BAR_CLASSES | ICC_PROGRESS_CLASS | ICC_STANDARD_CLASSES,
        };
        InitCommonControlsEx(&icc);
        // 让布局按物理像素算，避免被系统拉伸模糊
        let _ = windows_sys::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
        // 窗口创建前先用系统 DPI 给一个合理的初始尺寸；WM_CREATE 会再按
        // 窗口实际所在显示器校正一次。
        scale::set(win::system_dpi());

        // 注册主窗口类（大预览窗口已按用户要求移除）
        {
            let (name, proc_) = (
                MAIN_CLASS,
                wndproc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
            );
            let c = cls(name);
            let brush = theme::bg_brush();
            // 图标来自 exe 资源（IDI_ICON1 = 101，由 tools\embed-resources.ps1 注入）
            let app_icon = app_icon();
            let wc = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(proc_),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: win::module_handle(),
                hIcon: app_icon,
                hCursor: windows_sys::Win32::UI::WindowsAndMessaging::LoadCursorW(
                    std::ptr::null_mut(),
                    32512 as *const u16, // IDC_ARROW
                ),
                hbrBackground: brush as _,
                lpszMenuName: std::ptr::null(),
                lpszClassName: c.as_ptr(),
            };
            RegisterClassW(&wc);
        }

        let c = cls(MAIN_CLASS);
        // 窗口标题带版本号（v1.0.0 增补需求）
        let t = win::wide(&app_title());
        let hwnd = CreateWindowExW(
            0,
            c.as_ptr(),
            t.as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_THICKFRAME,
            100,
            80,
            scale::px(DEFAULT_W),
            scale::px(DEFAULT_H),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            win::module_handle(),
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return 2;
        }
        // 任务栏/标题栏图标（大图标给 Alt-Tab 与任务栏）
        let icon = app_icon();
        if !icon.is_null() {
            windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW(
                hwnd,
                0x0080,
                1,
                icon as isize,
            ); // WM_SETICON / ICON_BIG
            windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW(
                hwnd,
                0x0080,
                0,
                icon as isize,
            ); // ICON_SMALL
        }
        theme::dark_window_frame(hwnd);
        logline!("[start] 窗口创建完成 {} ms", t_start.elapsed().as_millis());
        // 命令行预先指定目标（便于脚本化验证，也方便用户直接从命令行带目标启动）
        if let Some(h) = initial_target {
            if let Some(app) = app_mut(hwnd) {
                app.target = Some(win::describe(h as HWND));
                app.pick_state = PickState::Ok;
                app.preview_cap =
                    ThumbnailCapture::new_scaled(h as HWND, scale::px(PREVIEW_MAX_W)).ok();
                if let Some(cap) = &mut app.preview_cap {
                    app.preview = cap.grab().ok();
                }
                logline!(
                    "[start] 命令行指定目标 0x{:X} | {:?} | 预览={}x{}",
                    h,
                    app.target.as_ref().map(|t| t.label()),
                    app.preview.as_ref().map(|f| f.width).unwrap_or(0),
                    app.preview.as_ref().map(|f| f.height).unwrap_or(0)
                );
                app.refresh_labels();
            }
        }
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
        SetWindowPos(
            hwnd,
            (-1isize) as HWND, // HWND_TOPMOST
            0,
            0,
            0,
            0,
            0x0002 | 0x0001 | 0x0010, // SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE
        );
        logline!("[start] 首帧显示 {} ms", t_start.elapsed().as_millis());
        SetTimer(hwnd, TIMER_UI, 500, None);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        0
    }
}

const MAIN_CLASS: &str = "CanLapseMain";

// ---------------------------------------------------------------------------
// 合成进度弹窗
// ---------------------------------------------------------------------------
//
// 需求（v1.0.0 收工后增补）：停止录制后立即恢复主窗口，并**弹出独立窗口**显示合成进度；
// 合成完成后弹窗自动消失（无需点【确定】）；风格与主界面统一（深色底 + 圆角 + 同字体）。
// 实现：带标题栏的小窗（owner = 主窗，置顶），内容全部自绘，进度从 EncState 的原子量现取。

const ENC_DLG_CLASS: &str = "CanLapseEncodeDialog";

struct EncDlg {
    main: HWND,
    progress: std::sync::Arc<std::sync::atomic::AtomicU32>,
    total: std::sync::Arc<std::sync::atomic::AtomicU32>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// 结束文案与出现时刻；None = 还在合成
    done: Option<(String, std::time::Instant)>,
}

static ENC_DLG_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// 弹窗里的【取消合成】按钮矩形（绘制与命中共用）
fn dlg_btn_rect(w: i32, h: i32) -> RECT {
    let bw = scale::px(108);
    let bh = scale::px(26);
    let top = h - scale::px(42);
    RECT {
        left: (w - bw) / 2,
        top,
        right: (w + bw) / 2,
        bottom: top + bh,
    }
}

unsafe fn enc_dlg_paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let dc = BeginPaint(hwnd, &mut ps);
    let mut rc: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut rc);
    let (w, h) = (rc.right, rc.bottom);
    theme::fill_round(dc, rc, theme::BG, 0);

    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut EncDlg;
    if ptr.is_null() {
        EndPaint(hwnd, &ps);
        return;
    }
    let dlg = &*ptr;
    let done = dlg.progress.load(Ordering::SeqCst);
    let total = dlg.total.load(Ordering::SeqCst);
    let pct = if total > 0 {
        (done as i64 * 100 / total as i64) as i32
    } else {
        0
    };

    SetBkMode(dc, 1);
    let old = SelectObject(dc, theme::heading_font());
    let title = if dlg.done.is_some() {
        lang::s(lang::K::DlgDone)
    } else {
        lang::s(lang::K::DlgExporting)
    };
    let title_rc = RECT {
        left: scale::px(18),
        top: scale::px(14),
        right: w - scale::px(18),
        bottom: scale::px(40),
    };
    theme::text(dc, title, title_rc, theme::TEXT, 0x0000_0004);

    // 进度条
    let bar_x = scale::px(18);
    let bar_w = (w - scale::px(36) - scale::px(54)).max(scale::px(80));
    let bar_top = scale::px(54);
    let track = RECT {
        left: bar_x,
        top: bar_top,
        right: bar_x + bar_w,
        bottom: bar_top + scale::px(12),
    };
    theme::fill_round(dc, track, theme::BORDER, 12);
    if pct > 0 {
        let fill_w = (bar_w as f64 * pct as f64 / 100.0).round() as i32;
        if fill_w > 2 {
            let f = RECT {
                left: bar_x,
                top: bar_top,
                right: bar_x + fill_w,
                bottom: bar_top + scale::px(12),
            };
            theme::fill_round(dc, f, theme::ACCENT, 12);
        }
    }
    let pct_rc = RECT {
        left: bar_x + bar_w + scale::px(8),
        top: bar_top - scale::px(4),
        right: w - scale::px(18),
        bottom: bar_top + scale::px(16),
    };
    theme::text(dc, &format!("{pct}%"), pct_rc, theme::TEXT, 0x0000_0004);

    // 帧数 / 结束信息
    let small = theme::small_font();
    let old2 = SelectObject(dc, small);
    let info = match &dlg.done {
        Some((t, _)) => t.clone(),
        None => lang::f(lang::K::DlgFrames, &[&done, &total]),
    };
    let info_rc = RECT {
        left: scale::px(18),
        top: bar_top + scale::px(24),
        right: w - scale::px(18),
        bottom: bar_top + scale::px(64),
    };
    theme::text_block(dc, &info, info_rc, theme::MUTED, 0x0000_0001 | 0x0000_0010); // DT_CENTER | DT_WORDBREAK
    SelectObject(dc, old2);

    // 取消按钮（合成中才显示）
    if dlg.done.is_none() {
        let cb = dlg_btn_rect(w, h);
        theme::fill_round(dc, cb, theme::PANEL, 10);
        let border = CreatePen(PS_SOLID, 1, theme::BORDER);
        let ob = SelectObject(dc, GetStockObject(NULL_BRUSH));
        let op = SelectObject(dc, border);
        RoundRect(dc, cb.left, cb.top, cb.right - 1, cb.bottom - 1, 10, 10);
        SelectObject(dc, op);
        SelectObject(dc, ob);
        DeleteObject(border);
        theme::text(
            dc,
            lang::s(lang::K::CancelExport),
            cb,
            theme::TEXT,
            0x0000_0001 | 0x0000_0004,
        );
    }

    SelectObject(dc, old);
    EndPaint(hwnd, &ps);
}

unsafe extern "system" fn enc_dlg_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    win::guard(|| match msg {
        WM_CREATE => 0,
        WM_PAINT => {
            enc_dlg_paint(hwnd);
            0
        }
        WM_ERASEBKGND => 1,
        WM_TIMER => {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut EncDlg;
            if !ptr.is_null() {
                if let Some((_, at)) = &(*ptr).done {
                    // 结果停留 1.6 秒后自动关闭（用户要求：不用点确定）
                    if at.elapsed().as_millis() >= 1600 {
                        DestroyWindow(hwnd);
                        return 0;
                    }
                }
            }
            InvalidateRect(hwnd, std::ptr::null(), 0);
            0
        }
        WM_LBUTTONUP => {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut EncDlg;
            if !ptr.is_null() && (*ptr).done.is_none() {
                let x = (lparam as i32) & 0xffff;
                let y = ((lparam as i32) >> 16) & 0xffff;
                let mut cr: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut cr);
                let cb = dlg_btn_rect(cr.right, cr.bottom);
                if x >= cb.left && x < cb.right && y >= cb.top && y < cb.bottom {
                    (*ptr).cancel.store(true, Ordering::SeqCst);
                    crate::logline!("[encode] 用户点了『取消合成』");
                }
            }
            0
        }
        WM_SETCURSOR => {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut EncDlg;
            if !ptr.is_null() && (*ptr).done.is_none() {
                let mut cr: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut cr);
                let cb = dlg_btn_rect(cr.right, cr.bottom);
                let mut p: POINT = std::mem::zeroed();
                windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut p);
                windows_sys::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut p);
                if p.x >= cb.left && p.x < cb.right && p.y >= cb.top && p.y < cb.bottom {
                    let hand = LoadCursorW(std::ptr::null_mut(), 32649 as *const u16); // IDC_HAND
                    SetCursor(hand);
                    return 1;
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_CLOSE => {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut EncDlg;
            if !ptr.is_null() && (*ptr).done.is_none() {
                (*ptr).cancel.store(true, Ordering::SeqCst);
            }
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut EncDlg;
            if !ptr.is_null() {
                let main = (*ptr).main;
                SetForegroundWindow(main);
                drop(Box::from_raw(ptr));
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            ENC_DLG_HWND.store(0, Ordering::SeqCst);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    })
}

/// 弹出"合成进度"窗口（已存在则复用）。
///
/// # Safety
///
/// 必须在**拥有 UI 消息泵的线程**上调用。`main` 句柄必须是有效的顶层窗口句柄。
/// `progress` / `total` / `cancel` 必须是跨 `Arc` 共享的原子量 —— 合成线程读、UI 线程写。
pub unsafe fn show_enc_dialog(
    main: HWND,
    progress: std::sync::Arc<std::sync::atomic::AtomicU32>,
    total: std::sync::Arc<std::sync::atomic::AtomicU32>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> HWND {
    static REG: std::sync::Once = std::sync::Once::new();
    REG.call_once(|| {
        let c = cls(ENC_DLG_CLASS);
        let wc = WNDCLASSW {
            style: 0x0002 | 0x0020, // CS_HREDRAW | CS_VREDRAW
            lpfnWndProc: Some(enc_dlg_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: win::module_handle(),
            hIcon: app_icon(),
            hCursor: LoadCursorW(std::ptr::null_mut(), 32512 as *const u16), // IDC_ARROW
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: c.as_ptr(),
        };
        RegisterClassW(&wc);
    });

    let old = ENC_DLG_HWND.load(Ordering::SeqCst);
    if old != 0 {
        let old_hwnd = old as HWND;
        let ptr = GetWindowLongPtrW(old_hwnd, GWLP_USERDATA) as *mut EncDlg;
        if !ptr.is_null() && (*ptr).done.is_none() && win::is_window_alive(old_hwnd) {
            return old_hwnd;
        }
        // 上一个弹窗已经处于“结果展示，等 1.6 秒自动关闭”的状态；
        // 新合成不能复用它的 Arc，否则进度永远不会刷新。
        DestroyWindow(old_hwnd);
    }

    let (dw, dh) = (scale::px(380), scale::px(196));
    let mut mr: RECT = std::mem::zeroed();
    GetWindowRect(main, &mut mr);
    // 居中到主窗口，但必须夹在工作区内 —— 主窗贴边时居中会跑到屏幕外（实测 x=-25）
    // 用主窗所在显示器，而不是 SPI_GETWORKAREA（后者永远是主屏）。
    let work = win::monitor_work_area(main);
    let margin = scale::px(8);
    let x = (mr.left + ((mr.right - mr.left) - dw) / 2).clamp(
        work.left + margin,
        (work.right - dw - margin).max(work.left + margin),
    );
    let y = (mr.top + ((mr.bottom - mr.top) - dh) / 2).clamp(
        work.top + margin,
        (work.bottom - dh - margin).max(work.top + margin),
    );

    let c = cls(ENC_DLG_CLASS);
    let t = win::wide(lang::s(lang::K::DlgTitle));
    let hwnd = CreateWindowExW(
        0x0000_0008, // WS_EX_TOPMOST
        c.as_ptr(),
        t.as_ptr(),
        0x8000_0000 | 0x00C0_0000 | 0x0008_0000, // WS_POPUP | WS_CAPTION | WS_SYSMENU
        x,
        y,
        dw,
        dh,
        main,
        std::ptr::null_mut(),
        win::module_handle(),
        std::ptr::null(),
    );
    if hwnd.is_null() {
        return std::ptr::null_mut();
    }
    let ctx = Box::new(EncDlg {
        main,
        progress,
        total,
        cancel,
        done: None,
    });
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(ctx) as isize);
    theme::dark_window_frame(hwnd);
    // 标题栏也用自己的图标（否则左上角是空白方块）
    let ic = app_icon();
    if !ic.is_null() {
        SendMessageW(hwnd, 0x0080, 1, ic as isize); // WM_SETICON / ICON_BIG
        SendMessageW(hwnd, 0x0080, 0, ic as isize); // ICON_SMALL
    }
    // 合成期间主窗不接受操作（弹窗是唯一交互面），关窗时在 WM_DESTROY 里恢复
    windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow(main, 0);
    ShowWindow(hwnd, SW_SHOW);
    UpdateWindow(hwnd);
    SetTimer(hwnd, 1, 200, None);
    ENC_DLG_HWND.store(hwnd as isize, Ordering::SeqCst);
    hwnd
}

/// 通知弹窗"合成结束"：显示结果文案，1.6 秒后自动关闭。
///
/// # Safety
///
/// 必须在**拥有 UI 消息泵的线程**上调用。内部通过 `PostMessage` 向
/// `ENC_DLG_HWND` 发送关闭消息 —— 如果那个 HWND 已被销毁（合成弹窗关闭时），
/// 消息会被 `PostMessage` 静默丢弃（这是我们期望的行为）。
pub unsafe fn finish_enc_dialog(text: &str) {
    let h = ENC_DLG_HWND.load(Ordering::SeqCst);
    if h == 0 {
        return;
    }
    let hwnd = h as HWND;
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut EncDlg;
    if ptr.is_null() {
        return;
    }
    if (*ptr).done.is_none() {
        (*ptr).done = Some((text.to_string(), std::time::Instant::now()));
        InvalidateRect(hwnd, std::ptr::null(), 1);
    }
}

#[cfg(test)]
mod tests {
    use super::{SENS_DEFAULT, SENS_LEVELS};

    #[test]
    fn 灵敏度档位按0点002递增() {
        assert_eq!(
            SENS_LEVELS,
            [0.016, 0.014, 0.012, 0.010, 0.008, 0.006, 0.002, 0.001]
        );
        assert_eq!(SENS_DEFAULT, 5);
    }
}
