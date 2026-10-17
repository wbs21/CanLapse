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

mod floatbar;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, DrawEdge, EndPaint,
    GetStockObject, InvalidateRect, ReleaseDC, SelectObject, SetBkMode, StretchDIBits, UpdateWindow,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BDR_SUNKENOUTER, BF_RECT, DEFAULT_GUI_FONT, DIB_RGB_COLORS,
    HGDIOBJ, PAINTSTRUCT, RGBQUAD, SRCCOPY,
};
use windows_sys::Win32::UI::Controls::{
    InitCommonControlsEx, INITCOMMONCONTROLSEX, ICC_BAR_CLASSES, ICC_PROGRESS_CLASS,
    ICC_STANDARD_CLASSES, PBM_SETPOS, PBM_SETRANGE32, TBM_SETPOS, TBM_SETRANGE, TBS_HORZ,
    TBS_NOTICKS,
};
use windows_sys::Win32::UI::Controls::Dialogs::{GetSaveFileNameW, OPENFILENAMEW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetWindowLongPtrW, IsWindow, MessageBoxW, MoveWindow, PostQuitMessage, RegisterClassW,
    SendMessageW, SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos, SetWindowTextW,
    ShowWindow, TranslateMessage, GWLP_USERDATA, MB_ICONINFORMATION, MB_OK, MSG, SW_MINIMIZE,
    SW_RESTORE, SW_SHOW, SWP_NOZORDER, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_GETMINMAXINFO,
    WM_HSCROLL, WM_PAINT, WM_SIZE, WM_TIMER, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_MINIMIZEBOX,
    WS_OVERLAPPED, WS_SYSMENU, WS_THICKFRAME, WS_VISIBLE,
};

/// windows-sys 没有导出 TBM_GETPOS（只导出了 SET），用官方消息值兜底
// ⚠️ TBM_GETPOS = WM_USER = 0x0400。
// 这里曾经写成 0x0408 —— 那是 **TBM_SETRANGEMAX**，导致每次滑动都把最大值改成 0：
// 表现为滑块位置"跳回"、且只能往左不能往右（往右被 max=0 挡住）。
const TBM_GETPOS_C: u32 = 0x0400;

use crate::capture::ThumbnailCapture;
use crate::encode::{self, EncodeOptions, EncodeOutcome, Progress, Quality};
use crate::frame::Frame;
use crate::logline;
use crate::recorder::{self, Phase, RecorderConfig, Shared};
use crate::session::Session;
use crate::win;

// ---------------------------------------------------------------------------
// 控件 ID
// ---------------------------------------------------------------------------

const ID_PICK: usize = 100;
const ID_PREVIEW: usize = 101;
const ID_HELP: usize = 102;
const ID_STATUS: usize = 103;
const ID_HDR_REC: usize = 110;
const ID_LBL_FPS: usize = 111;
const ID_SLD_FPS: usize = 112;
const ID_LBL_THR: usize = 113;
const ID_SLD_THR: usize = 114;
const ID_HDR_EXP: usize = 120;
const ID_TIP: usize = 121;
const ID_CHK_CLEAN: usize = 122;
const ID_LBL_EFPS: usize = 123;
const ID_CMB_FPS: usize = 124;
const ID_LBL_QUAL: usize = 125;
const ID_CMB_QUAL: usize = 126;
const ID_LBL_OUT: usize = 127;
const ID_EDIT_OUT: usize = 128;
const ID_BTN_OUT: usize = 129;
const ID_HDR_HIST: usize = 130;
const ID_LIST: usize = 131;
const ID_REEXPORT: usize = 132;
const ID_OPEN_DIR: usize = 133;
const ID_DELETE: usize = 134;
const ID_RECORD: usize = 140;
const ID_STATE: usize = 141;
const ID_PROGRESS: usize = 142;
const ID_CANCEL: usize = 143;

const TIMER_UI: usize = 1;
const PREVIEW_W: i32 = 160;
const PREVIEW_H: i32 = 100;
const DEFAULT_W: i32 = 460;
const DEFAULT_H: i32 = 380;
const MIN_W: i32 = 400;
const MIN_H: i32 = 320;

fn cls(s: &str) -> Vec<u16> {
    win::wide(s)
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum PickState {
    Empty,
    Ok,
    /// 画布没浮动（窗口里含工具栏/面板）—— 只做提醒，允许强行继续
    HasPanel,
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
}

#[derive(Default)]
struct Ctl {
    title: HWND,
    btn_pick: HWND,
    btn_preview: HWND,
    btn_help: HWND,
    status: HWND,
    hdr_rec: HWND,
    lbl_fps: HWND,
    sld_fps: HWND,
    lbl_thr: HWND,
    sld_thr: HWND,
    hdr_exp: HWND,
    tip: HWND,
    chk_clean: HWND,
    lbl_efps: HWND,
    cmb_fps: HWND,
    lbl_qual: HWND,
    cmb_qual: HWND,
    lbl_out: HWND,
    edit_out: HWND,
    btn_out: HWND,
    hdr_hist: HWND,
    list: HWND,
    btn_reexport: HWND,
    btn_open: HWND,
    btn_delete: HWND,
    btn_record: HWND,
    state: HWND,
    progress: HWND,
    btn_cancel: HWND,
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
    export_fps: f64,
    quality: Quality,
    auto_clean: bool,
    out_path: PathBuf,
    target: Option<win::WindowInfo>,
    preview: Option<Frame>,
    preview_rect: RECT,
    pick_state: PickState,
    preview_cap: Option<ThumbnailCapture>,
    sessions: Vec<crate::session::SessionSummary>,
    sel: Option<usize>,
    shared: Option<Arc<Shared>>,
    worker: Option<JoinHandle<()>>,
    bar: HWND,
    enc: Option<EncState>,
    relayouting: bool,
    preview_win: HWND,
    /// 预览抓帧失败的上一次记录：`(错误串, 时间)` —— 目标窗口关闭/最小化时
    /// 这个错误会每 500ms 重复一次，只在"错误变了或过了 10 秒"时写日志，避免刷屏。
    preview_err: Option<(String, std::time::Instant)>,
    /// 上一次成功抓到的预览尺寸，尺寸变化时记一条日志（换目标/改窗口大小时有用）
    preview_size: (i32, i32),
}

impl App {
    fn status_text(&self) -> String {
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
            PickState::Empty => "尚未选择画布窗口".to_string(),
            PickState::Ok => format!("✅ 已选中画布窗口（{picked}）"),
            PickState::HasPanel => {
                "⚠️ 检测到窗口包含工具栏，画布未浮动（点【如何定位窗口】查看做法）".to_string()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 布局
// ---------------------------------------------------------------------------

fn rect(x: i32, y: i32, w: i32, h: i32) -> RECT {
    RECT { left: x, top: y, right: x + w, bottom: y + h }
}

/// 计算所有控件的目标位置，并返回（内容所需高度, 预览矩形）
fn compute_layout(app: &App, cw: i32) -> (Vec<(HWND, RECT, bool)>, RECT, i32) {
    let m = 10;
    let inner = (cw - m * 2).max(120);
    let mut y = 8;
    let mut out: Vec<(HWND, RECT, bool)> = Vec::new();
    let c = &app.ctl;
    let mut push = |h: HWND, r: RECT, vis: bool| out.push((h, r, vis));

    push(c.title, rect(m, y, inner, 20), true);
    y += 24;

    // 拾取模块按钮组
    let gap = 6;
    let bw = (inner - gap * 2) / 3;
    push(c.btn_pick, rect(m, y, bw, 26), true);
    push(c.btn_preview, rect(m + bw + gap, y, bw, 26), true);
    push(c.btn_help, rect(m + (bw + gap) * 2, y, inner - (bw + gap) * 2, 26), true);
    y += 32;

    // 预览缩略图（固定 160×100，规格值）+ 右侧的【开始录制】
    // 尺寸与顶部三个按钮一致（同宽同高），并且**右对齐**到内容右边界
    let prev = rect(m, y, PREVIEW_W, PREVIEW_H);
    push(
        c.btn_record,
        rect(cw - m - bw, y + PREVIEW_H / 2 - 13, bw, 26),
        true,
    );
    y += PREVIEW_H + 6;

    push(c.status, rect(m, y, inner, 18), true);
    y += 24;

    // 录制参数（不折叠，直接显示）。折叠标题栏控件保留但隐藏、不占高度。
    push(c.hdr_rec, rect(m, y, inner, 22), false);
    push(c.lbl_fps, rect(m + 8, y, inner - 16, 16), true);
    y += 18;
    push(c.sld_fps, rect(m + 8, y, inner - 16, 24), true);
    y += 26;
    push(c.lbl_thr, rect(m + 8, y, inner - 16, 16), true);
    y += 18;
    push(c.sld_thr, rect(m + 8, y, inner - 16, 24), true);
    y += 30;

    // 导出视频参数（不折叠，直接显示）
    push(c.hdr_exp, rect(m, y, inner, 22), false);
    push(c.tip, rect(m + 8, y, inner - 16, 32), true);
    y += 34;
    push(c.chk_clean, rect(m + 8, y, inner - 16, 20), true);
    y += 24;
    push(c.lbl_efps, rect(m + 8, y, 70, 20), true);
    push(c.cmb_fps, rect(m + 80, y, 80, 22), true);
    push(c.lbl_qual, rect(m + 170, y, 50, 20), true);
    push(c.cmb_qual, rect(m + 220, y, inner - 220 + m - 8, 22), true);
    y += 28;
    push(c.lbl_out, rect(m + 8, y, 40, 20), true);
    push(c.edit_out, rect(m + 48, y, inner - 48 - 60, 22), true);
    push(c.btn_out, rect(cw - m - 52, y, 52, 22), true);
    y += 28;

    // 历史缓存会话：按用户要求移除整块（控件保留但永久隐藏，不占高度）
    push(c.hdr_hist, rect(m, y, inner, 22), false);
    push(c.list, rect(m, y, inner - 16, 96), false);
    push(c.btn_reexport, rect(m, y, 80, 24), false);
    push(c.btn_open, rect(m, y, 80, 24), false);
    push(c.btn_delete, rect(m, y, 80, 24), false);

    // 合成进度 + 取消合成（开始录制已移到预览区右侧）
    let by = y.max(0) + 2;
    push(c.progress, rect(m, by, inner - 76, 20), true);
    push(c.btn_cancel, rect(cw - m - 64, by, 64, 24), true);
    // 状态栏：贴窗口最底部、整宽（最后一行）
    let status_h = 20;
    let need = by + 26 + status_h;
    push(c.state, rect(0, need - status_h, cw, status_h), true);

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

unsafe extern "system" fn wndproc(
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
        WM_GETMINMAXINFO => {
            let mmi = lparam as *mut windows_sys::Win32::UI::WindowsAndMessaging::MINMAXINFO;
            (*mmi).ptMinTrackSize.x = MIN_W;
            (*mmi).ptMinTrackSize.y = MIN_H;
            0
        }
        // 【开始录制】绿色底 + 白字。WM_CTLCOLORSTATIC = 0x0138：
        // wparam 是 DC，lparam 是控件句柄。带视觉主题的普通按钮会忽略
        // WM_CTLCOLORBTN，所以按钮改用 Static（静态控件认这个消息）。
        0x0138 => {
            if lparam as HWND
                == windows_sys::Win32::UI::WindowsAndMessaging::GetDlgItem(
                    hwnd,
                    ID_RECORD as i32,
                )
            {
                use windows_sys::Win32::Graphics::Gdi::{CreateSolidBrush, SetBkMode, SetTextColor};
                SetBkMode(wparam as _, 1); // TRANSPARENT
                SetTextColor(wparam as _, 0x00FF_FFFF);
                static GREEN: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
                let brush = *GREEN.get_or_init(|| CreateSolidBrush(0x0033_8A2E) as usize);
                return brush as isize;
            }
            // 其它静态控件必须交回默认处理，否则返回 0 会让它们的背景被涂黑
            // （实测：滑块/标签背景整条变黑）
            windows_sys::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, 0x0138, wparam, lparam)
        }
        WM_COMMAND => {
            let id = (wparam & 0xffff) as usize;
            let code = ((wparam >> 16) & 0xffff) as u32;
            if let Some(app) = app_mut(hwnd) {
                app.on_command(id, code);
            }
            0
        }
        WM_HSCROLL => {
            if let Some(app) = app_mut(hwnd) {
                app.sync_sliders();
            }
            0
        }
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
        let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
        let root = default_cache_root();
        App {
            hwnd,
            ctl: Ctl::default(),
            font,
            rec_open: true,
            exp_open: true, // 按要求：导出参数不再折叠，直接显示
            hist_open: false,
            fps: 2.0,
            threshold: 0.01,
            export_fps: 24.0,
            quality: Quality::High,
            auto_clean: true,
            out_path: root.join("timelapse.mp4"),
            target: None,
            preview: None,
            preview_rect: rect(0, 0, 0, 0),
            pick_state: PickState::Empty,
            preview_cap: None,
            sessions: Vec::new(),
            sel: None,
            shared: None,
            worker: None,
            bar: std::ptr::null_mut(),
            enc: None,
            relayouting: false,
            preview_win: std::ptr::null_mut(),
            preview_err: None,
            preview_size: (0, 0),
        }
    }

    unsafe fn build_controls(&mut self) {
        let h = self.hwnd;
        let f = self.font;
        let ws = WS_CHILD | WS_VISIBLE;
        let mut c = Ctl::default();
        c.title = mk(h, "Static", "CanLapse · 画布缩时录屏", ws, 0, f);
        c.btn_pick = mk(h, "Button", "🎯 拾取窗口", ws, ID_PICK, f);
        c.btn_preview = mk(h, "Button", "🔍 预览窗口", ws, ID_PREVIEW, f);
        c.btn_help = mk(h, "Button", "❓ 如何定位窗口", ws, ID_HELP, f);
        c.status = mk(h, "Static", "", ws, ID_STATUS, f);

        c.hdr_rec = mk(h, "Button", "▼ 录制参数", ws, ID_HDR_REC, f);
        c.lbl_fps = mk(h, "Static", "", ws, ID_LBL_FPS, f);
        c.sld_fps = mk(h, "msctls_trackbar32", "", ws | TBS_HORZ | TBS_NOTICKS, ID_SLD_FPS, f);
        c.lbl_thr = mk(h, "Static", "", ws, ID_LBL_THR, f);
        c.sld_thr = mk(h, "msctls_trackbar32", "", ws | TBS_HORZ | TBS_NOTICKS, ID_SLD_THR, f);
        SendMessageW(c.sld_fps, TBM_SETRANGE, 1, pack(1, 8));
        SendMessageW(c.sld_fps, TBM_SETPOS, 1, 2);
        SendMessageW(c.sld_thr, TBM_SETRANGE, 1, pack(0, 50));
        SendMessageW(c.sld_thr, TBM_SETPOS, 1, 10);

        c.hdr_exp = mk(h, "Button", "▶ 导出视频参数", ws, ID_HDR_EXP, f);
        c.tip = mk(
            h,
            "Static",
            "💡提示：如果取消勾选「合成成功自动清理缓存」，PNG帧图片会保留在缓存目录，占用磁盘空间，请记得定期手动删除缓存。",
            ws,
            ID_TIP,
            f,
        );
        c.chk_clean = mk(h, "Button", "合成成功自动清理缓存", ws | 0x0000_0003, ID_CHK_CLEAN, f);
        SendMessageW(c.chk_clean, 0x00F1, 1, 0); // BM_SETCHECK = BST_CHECKED
        c.lbl_efps = mk(h, "Static", "播放帧率", ws, ID_LBL_EFPS, f);
        c.cmb_fps = mk(h, "ComboBox", "", ws | 0x0003 | 0x0020_0000, ID_CMB_FPS, f); // CBS_DROPDOWNLIST|WS_VSCROLL
        for rate in ["5 fps", "10 fps", "15 fps", "20 fps", "24 fps"] {
            let s = win::wide(rate);
            SendMessageW(c.cmb_fps, 0x0143, 0, s.as_ptr() as isize); // CB_ADDSTRING
        }
        SendMessageW(c.cmb_fps, 0x014E, 4, 0); // CB_SETCURSEL = 4（24 fps 默认）
        c.lbl_qual = mk(h, "Static", "画质", ws, ID_LBL_QUAL, f);
        c.cmb_qual = mk(h, "ComboBox", "", ws | 0x0003 | 0x0020_0000, ID_CMB_QUAL, f);
        for q in ["高", "中", "低"] {
            let s = win::wide(q);
            SendMessageW(c.cmb_qual, 0x0143, 0, s.as_ptr() as isize);
        }
        SendMessageW(c.cmb_qual, 0x014E, 0, 0);
        c.lbl_out = mk(h, "Static", "输出", ws, ID_LBL_OUT, f);
        c.edit_out = mk(h, "Edit", "", ws | 0x0080, ID_EDIT_OUT, f); // ES_AUTOHSCROLL
        c.btn_out = mk(h, "Button", "选择", ws, ID_BTN_OUT, f);

        c.hdr_hist = mk(h, "Button", "▶ 历史缓存会话", ws, ID_HDR_HIST, f);
        c.list = mk(h, "ListBox", "", ws | 0x0001 | 0x0020_0000 | 0x0080_0000, ID_LIST, f); // LBS_NOTIFY|WS_VSCROLL|WS_BORDER
        c.btn_reexport = mk(h, "Button", "重新导出", ws, ID_REEXPORT, f);
        c.btn_open = mk(h, "Button", "打开文件夹", ws, ID_OPEN_DIR, f);
        c.btn_delete = mk(h, "Button", "删除", ws, ID_DELETE, f);

        c.state = mk(h, "Static", "待机", ws, ID_STATE, f);
        // 【开始录制】用 Static 而非常规按钮：带视觉主题的按钮会忽略 WM_CTLCOLORBTN，
        // 而静态控件认 WM_CTLCOLORSTATIC，才能可靠地画成绿底白字。
        // SS_CENTER(0x1) | SS_NOTIFY(0x100) | SS_CENTERIMAGE(0x200)
        c.btn_record = mk(
            h,
            "Static",
            "开始录制",
            ws | 0x0000_0001 | 0x0000_0100 | 0x0000_0200,
            ID_RECORD,
            f,
        );
        c.progress = mk(h, "msctls_progress32", "", ws, ID_PROGRESS, f);
        c.btn_cancel = mk(h, "Button", "取消合成", ws, ID_CANCEL, f);
        SendMessageW(c.progress, PBM_SETRANGE32, 0, 100);
        self.ctl = c;

        // 初始可见性：拾取参数展开、导出/历史折叠
        self.set_panel_visible();
        let s = win::wide(&format!("{}", self.out_path.display()));
        SetWindowTextW(self.ctl.edit_out, s.as_ptr());
        self.refresh_session_list();
        self.refresh_labels();
        self.ui_set_idle();
    }

    unsafe fn set_panel_visible(&self) {
        let c = &self.ctl;
        let show = |h: HWND, v: bool| {
            ShowWindow(h, if v { SW_SHOW } else { 0 });
        };
        for h in [c.lbl_fps, c.sld_fps, c.lbl_thr, c.sld_thr] {
            show(h, self.rec_open);
        }
        for h in [
            c.tip, c.chk_clean, c.lbl_efps, c.cmb_fps, c.lbl_qual, c.cmb_qual, c.lbl_out,
            c.edit_out, c.btn_out,
        ] {
            show(h, self.exp_open);
        }
        for h in [c.list, c.btn_reexport, c.btn_open, c.btn_delete] {
            show(h, self.hist_open);
        }
    }

    unsafe fn refresh_labels(&self) {
        let c = &self.ctl;
        let t = win::wide(&format!("帧密度：{} fps", self.fps));
        SetWindowTextW(c.lbl_fps, t.as_ptr());
        let t = win::wide(&format!("像素差异阈值：{:.3}", self.threshold));
        SetWindowTextW(c.lbl_thr, t.as_ptr());
        let t = win::wide(&self.status_text());
        SetWindowTextW(c.status, t.as_ptr());
    }

    unsafe fn sync_sliders(&mut self) {
        let c = &self.ctl;
        // 保险：控件句柄异常时直接返回，不要让读位置引发崩溃
        if c.sld_fps.is_null() || c.sld_thr.is_null() {
            return;
        }
        let f = SendMessageW(c.sld_fps, TBM_GETPOS_C, 0, 0) as i32;
        let t = SendMessageW(c.sld_thr, TBM_GETPOS_C, 0, 0) as i32;
        self.fps = (f.clamp(1, 8)) as f64;
        self.threshold = (t.clamp(0, 50)) as f32 / 1000.0;
        self.refresh_labels();
    }

    /// 重排控件；`initial` 时允许调整窗口尺寸
    unsafe fn relayout(&mut self, initial: bool) {
        if self.relayouting {
            return;
        }
        self.relayouting = true;
        let mut cr: RECT = std::mem::zeroed();
        GetClientRect(self.hwnd, &mut cr);
        let cw = (cr.right - cr.left).max(MIN_W);
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
        // 【开始录制】小圆角：它是 Static（矩形），用窗口区域裁成圆角，
        // 免得再去走 BS_OWNERDRAW 那条会崩的自绘路线。
        {
            let bw = (cw - 20 - 12) / 3;
            use windows_sys::Win32::Graphics::Gdi::{CreateRoundRectRgn, SetWindowRgn};
            let rgn = CreateRoundRectRgn(0, 0, bw + 1, 27, 8, 8);
            SetWindowRgn(self.ctl.btn_record, rgn, 1);
        }
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
                (wr.right - wr.left).max(DEFAULT_W),
                target,
                SWP_NOZORDER | 0x0004, // SWP_NOACTIVATE
            );
        } else if initial {
            let _ = SetWindowPos(
                self.hwnd,
                std::ptr::null_mut(),
                0,
                0,
                DEFAULT_W,
                DEFAULT_H,
                SWP_NOZORDER | 0x0004,
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
            let text = format!("{} ｜ {} 帧", s.name, s.frames);
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
                ID_PICK => self.do_pick(),
                ID_HELP => self.show_help(),
                ID_PREVIEW => self.show_big_preview(),
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
                ID_CMB_FPS => {
                    let i = SendMessageW(self.ctl.cmb_fps, 0x0147, 0, 0) as i32; // CB_GETCURSEL
                    self.export_fps = [5.0, 10.0, 15.0, 20.0, 24.0]
                        .get(i.max(0) as usize)
                        .copied()
                        .unwrap_or(10.0);
                }
                ID_CMB_QUAL => {
                    let i = SendMessageW(self.ctl.cmb_qual, 0x0147, 0, 0) as i32;
                    self.quality = match i {
                        1 => Quality::Medium,
                        2 => Quality::Low,
                        _ => Quality::High,
                    };
                }
                ID_BTN_OUT => self.choose_output(),
                ID_RECORD => {
                    let running = self.shared.as_ref().map(|s| s.phase().is_running()).unwrap_or(false);
                    if running {
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
        set(self.ctl.hdr_rec, self.rec_open, "录制参数");
        set(self.ctl.hdr_exp, self.exp_open, "导出视频参数");
        set(self.ctl.hdr_hist, self.hist_open, "历史缓存会话");
    }

    unsafe fn do_pick(&mut self) {
        // 拾取期间：**始终置顶 + 半透明**（按用户要求）。
        // 置顶保证提示始终可见；半透明让我们不完全挡住下面的目标，方便点它的标题栏。
        // GWL_EXSTYLE = -20，WS_EX_LAYERED = 0x00080000，LWA_ALPHA = 0x2
        let ex = GetWindowLongPtrW(self.hwnd, -20) as i64;
        // 按用户要求：**不再半透明、不再鼠标穿透**，改为拾取时最小化。
        // 这里只把扩展样式保持原样（清掉 LAYERED/TRANSPARENT 位以防残留）。
        SetWindowLongPtrW(self.hwnd, -20, (ex & !(0x0008_0000 | 0x0000_0020)) as isize);
        windows_sys::Win32::UI::WindowsAndMessaging::SetLayeredWindowAttributes(
            self.hwnd, 0, 190, 0x0000_0002,
        );
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
        // 说明清楚：拾取时 CanLapse 已最小化到任务栏（点不到），唯一该点的是画布。
        let t = win::wide("CanLapse 已最小化到任务栏；请点击画布画面（按 Esc 取消）");
        SetWindowTextW(self.ctl.state, t.as_ptr());
        UpdateWindow(self.hwnd);

        let hint = win::pick_hint_create("CanLapse 已最小化到任务栏 · 请点画布画面确认（Esc 取消）");
        let picked = win::pick_window_interactive(self.hwnd, Some(hint));
        win::pick_hint_destroy(hint);

        // 拾取结束：取消半透明与置顶，把界面拉回最前并正常显示
        // SWP_NOMOVE|NOSIZE
        SetWindowPos(self.hwnd, std::ptr::null_mut(), 0, 0, 0, 0, 0x0002 | 0x0001);
        let ex2 = GetWindowLongPtrW(self.hwnd, -20) as i64;
        // 去掉 LAYERED 与 TRANSPARENT，恢复正常可点击
        SetWindowLongPtrW(self.hwnd, -20, (ex2 & !(0x0008_0000 | 0x0000_0020)) as isize);
        ShowWindow(self.hwnd, SW_RESTORE);
        SetForegroundWindow(self.hwnd);
        InvalidateRect(self.hwnd, std::ptr::null(), 1);

        let Some(hwnd) = picked else {
            logline!("[pick] 拾取取消（Esc / 主窗口被恢复 / 点在本程序窗口上）");
            self.refresh_labels();
            let t = win::wide("已取消拾取，请重新点【拾取窗口】");
            SetWindowTextW(self.ctl.state, t.as_ptr());
            return;
        };
        // ⚠️ 这里**不要**把目标"统一解析到顶层窗口"。
        // 旧写法 `root_of(hwnd)` 会把用户点中的"画布子窗口"换成整个主窗口 ——
        // 预览里于是出现工具栏和颜色面板，用户看到的就是"没锁到画布"（SAI2 上实测）。
        // 子窗口由 ThumbnailCapture::new 自己解析成「顶层缩略图 + rcSource 裁剪」。
        let info = win::describe(hwnd);
        let (cw, ch) = info.client_size();
        logline!(
            "[pick] 目标 0x{:X} | {} | 客户区 {}x{} | root=0x{:X} | 层级：{}",
            hwnd as isize,
            info.label(),
            cw,
            ch,
            win::root_of(hwnd) as isize,
            win::ancestor_chain(hwnd)
        );
        // 释放上一个预览抓帧器
        self.preview_cap = None;
        match ThumbnailCapture::new(hwnd) {
            Ok(cap) => {
                let (ow, oh) = cap.size();
                self.preview = match cap.grab() {
                    Ok(f) => {
                        logline!("[pick] 抓帧器 ok，输出 {}x{}，首帧 {}x{}", ow, oh, f.width, f.height);
                        Some(f)
                    }
                    Err(e) => {
                        logline!("[pick] 抓帧器 ok，但首帧失败：{e}");
                        None
                    }
                };
                self.preview_cap = Some(cap);
            }
            Err(e) => {
                // 不再弹模态框：把原因写进状态栏，用户可直接再点一次【拾取窗口】
                // ⚠️ 必须把旧预览清掉：否则状态栏说"抓不到"，画面上却还留着上一个目标的图，
                //    用户会以为拾取成功了（实测踩过）。
                logline!("[pick] 建抓帧器失败：{e}");
                self.preview = None;
                let t = win::wide(&format!("无法抓取该窗口：{e}　请重新拾取"));
                SetWindowTextW(self.ctl.state, t.as_ptr());
                InvalidateRect(self.hwnd, &self.preview_rect, 0);
                return;
            }
        }
        // 辅助提醒：目标客户区明显大于"典型画布比例"时提示可能没浮动
        // （规格：仅提示，允许强行继续）
        self.pick_state = if looks_like_panel(&info) {
            PickState::HasPanel
        } else {
            PickState::Ok
        };
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

    unsafe fn show_help(&self) {
        message(
            self.hwnd,
            "如何定位画布窗口",
            "想要只录制画布，需要把画布拆成独立浮动窗口，再使用【拾取窗口】选中画布。\n\n\
             SAI2：视图 → 新建浮动视图\n\
             Photoshop：拖拽画布标签向外拉出，脱离主框架\n\
             CSP / 优动漫 PAINT：拖拽画布标签分离，或 视图 → 新建视图\n\
             Krita：窗口 → 新建窗口\n\n\
             💡提示：拾取后查看预览窗口，如果画面包含工具栏 / 图层面板，说明画布没有浮动。\n\n\
             ℹ️ 关于遮挡：绘画软件**自己的**浮动面板（SAI2 的「颜色」等）若压在画布上，\n\
             会照原样录进画面 —— 这是正常的，**不做裁切、也不提示**。\n\
             而当你切到**其他软件**时，说明此刻没有在画画，录制会**自动暂停**，切回画布即继续。",
        );
    }

    unsafe fn show_big_preview(&mut self) {
        let Some(frame) = &self.preview else {
            message(self.hwnd, "预览", "还没有拾取窗口，先点【🎯 拾取窗口】。");
            return;
        };
        // 弹出一个可缩放的大图窗口，等比铺满客户区
        let (w, h) = (frame.width, frame.height);
        let scale = 2.0f64.min(900.0 / w as f64).max(0.2);
        let (ww, wh) = ((w as f64 * scale) as i32 + 20, (h as f64 * scale) as i32 + 50);
        if !self.preview_win.is_null() && IsWindow(self.preview_win) != 0 {
            DestroyWindow(self.preview_win);
        }
        let c = cls(PREVIEW_CLASS);
        let t = win::wide("预览窗口（等比缩放）");
        let hwnd = CreateWindowExW(
            0,
            c.as_ptr(),
            t.as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_VISIBLE | WS_THICKFRAME,
            200,
            150,
            ww,
            wh,
            self.hwnd,
            std::ptr::null_mut(),
            win::module_handle(),
            std::ptr::null(),
        );
        if !hwnd.is_null() {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, self as *mut App as isize);
            ShowWindow(hwnd, SW_SHOW);
            UpdateWindow(hwnd);
            // 必须主动提到前台：否则会创建在绘画软件**后面**，用户以为"预览打不开"
            // （实测踩过：截图抓到的是压在上面的别的窗口）
            SetForegroundWindow(hwnd);
        }
        self.preview_win = hwnd;
    }

    unsafe fn choose_output(&mut self) {
        let mut buf = [0u16; 1024];
        let init: Vec<u16> = self
            .out_path
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        buf[..init.len().min(1024)].copy_from_slice(&init[..init.len().min(1024)]);
        let filter: Vec<u16> = win::wide("MP4 视频\0*.mp4\0所有文件\0*.*\0");
        let defext = win::wide("mp4");
        let mut ofn: OPENFILENAMEW = std::mem::zeroed();
        ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        ofn.hwndOwner = self.hwnd;
        ofn.lpstrFilter = filter.as_ptr();
        ofn.lpstrFile = buf.as_mut_ptr();
        ofn.nMaxFile = buf.len() as u32;
        ofn.lpstrDefExt = defext.as_ptr();
        ofn.Flags = 0x0000_0002 | 0x0000_0008; // OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST
        if GetSaveFileNameW(&mut ofn) != 0 {
            let s = String::from_utf16_lossy(
                &buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())],
            );
            self.out_path = PathBuf::from(s);
            let t = win::wide(&self.out_path.to_string_lossy());
            SetWindowTextW(self.ctl.edit_out, t.as_ptr());
        }
    }

    unsafe fn start_recording(&mut self) {
        let Some(target) = &self.target else {
            message(self.hwnd, "提示", "请先点【🎯 拾取窗口】选择要录制的画布窗口。");
            return;
        };
        if !crate::capture::composition_enabled() {
            message(self.hwnd, "无法录制", "桌面合成（DWM）未开启，缩略图抓帧不可用。");
            return;
        }
        self.preview_cap = None; // 录制期间由录制线程独占抓帧
        logline!(
            "[record] 开始录制 target=0x{:X} | {} | fps={} threshold={}",
            target.hwnd as isize,
            target.label(),
            self.fps,
            self.threshold
        );
        let cfg = RecorderConfig {
            hwnd: target.hwnd as isize,
            root: default_cache_root(),
            name: if target.process.is_empty() {
                "画布".to_string()
            } else {
                target.process.clone()
            },
            fps: self.fps,
            threshold: self.threshold,
            fill_rgb: [0, 0, 0],
            // GUI 流程启用"其他软件在前台就暂停"（用户点完录制会点进画布）
            fg_pause: true,
        };
        let shared = Shared::new();
        let worker = recorder::spawn(cfg, shared.clone());
        self.shared = Some(shared.clone());
        self.worker = Some(worker);
        self.bar = floatbar::create(shared).unwrap_or(std::ptr::null_mut());
        // 规格：开始录制后主窗口自动最小化，只留悬浮条
        self.ui_set_recording();
        ShowWindow(self.hwnd, SW_MINIMIZE);
        SetTimer(self.hwnd, TIMER_UI, 200, None);
    }

    unsafe fn stop_and_encode(&mut self) {
        if let Some(w) = self.worker.take() {
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
            self.preview_cap = ThumbnailCapture::new(t.hwnd).ok();
        }

        let failed = shared.as_ref().map(|s| s.error()).flatten();
        logline!(
            "[record] 停止：dir={:?} frames={} error={:?}",
            dir,
            shared.as_ref().map(|s| s.frames()).unwrap_or(0),
            failed
        );
        if let Some(e) = failed {
            message(self.hwnd, "录制失败", &e);
            return;
        }
        let Some(dir) = dir else { return };
        let frames = shared.as_ref().map(|s| s.frames()).unwrap_or(0);
        if frames == 0 {
            message(self.hwnd, "录制结束", "这次没有保存到任何帧（画面可能始终没有变化）。");
            return;
        }
        // 规格：停止录制 → 进入视频合成流程（这条路径才适用"合成成功自动清理缓存"）
        let out = self.out_path.clone();
        let clean = self.auto_clean;
        self.start_encoding(dir, out, clean);
    }

    unsafe fn start_encoding(&mut self, session: PathBuf, out: PathBuf, clean_after: bool) {
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
        };
        self.enc = Some(st);
        let opts = EncodeOptions {
            fps: self.export_fps,
            quality: self.quality,
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
        std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let r = encode::encode_session(&dir, &opts, &mut |done, all| {
                progress.store(done, Ordering::SeqCst);
                total.store(all, Ordering::SeqCst);
                if cancel.load(Ordering::SeqCst) {
                    Progress::Cancel
                } else {
                    Progress::Continue
                }
            });
            crate::log::line(&format!(
                "[encode] 结束（{:.1}s）: {:?}",
                started.elapsed().as_secs_f64(),
                r.as_ref().map(|o| match o {
                    EncodeOutcome::Done(s) =>
                        format!("Done {}帧 {}x{} {:.1}s {}字节", s.frames, s.width, s.height, s.seconds, s.bytes),
                    EncodeOutcome::Cancelled(s) =>
                        format!("Cancelled {}帧 {}x{}", s.frames, s.width, s.height),
                })
            ));
            if let Ok(mut slot) = outcome.lock() {
                *slot = Some(r);
            }
            finished.store(true, Ordering::SeqCst);
        });
        let t = win::wide("正在准备合成…");
        SetWindowTextW(self.ctl.state, t.as_ptr());
        self.ui_set_encoding();
        SetTimer(self.hwnd, TIMER_UI, 200, None);
    }

    unsafe fn reexport_selected(&mut self) {
        let Some(s) = self.selected_session() else {
            message(self.hwnd, "提示", "请先在列表里选择一个会话。");
            return;
        };
        let dir = s.dir.clone();
        // 输出放在会话目录**外面**，并带帧率后缀，避免与"重新导出/清缓存"互相踩：
        // 旧写法 out = dir/timelapse.mp4 + 自动清理 = 刚导出的视频被自己删掉。
        let stem = s.name.clone();
        let out = dir
            .parent()
            .map(|p| p.join(format!("{stem}_{}fps.mp4", self.export_fps as i32)))
            .unwrap_or_else(|| dir.join("timelapse.mp4"));
        // 重新导出面向的是"已有缓存"，不应该清理它
        self.start_encoding(dir, out, false);
    }

    unsafe fn delete_selected(&mut self) {
        let Some(s) = self.selected_session() else {
            message(self.hwnd, "提示", "请先在列表里选择一个会话。");
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
            message(self.hwnd, "无法删除", "正在录制或正在合成中的会话不能删除。");
            return;
        }
        // 规格：删除会话会删除整个会话缓存目录
        if !confirm(self.hwnd, &format!("确定删除会话「{name}」及其全部 PNG 帧吗？")) {
            return;
        }
        match Session::delete(&dir) {
            Ok(_) => {
                self.refresh_session_list();
                self.relayout(false);
            }
            Err(e) => message(self.hwnd, "删除失败", &e),
        }
    }

    unsafe fn on_timer(&mut self) {
        // 录制状态
        if let Some(shared) = &self.shared {
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
            let pct = if total > 0 { (done * 100 / total) as isize } else { 0 };
            SendMessageW(self.ctl.progress, PBM_SETPOS, pct as usize, 0);
            let t = win::wide(&format!(
                "视频合成中… 已处理：{} / {} 帧",
                done, total
            ));
            SetWindowTextW(self.ctl.state, t.as_ptr());
            if e.finished.load(Ordering::SeqCst) {
                let outcome = e.outcome.lock().ok().and_then(|mut o| o.take());
                let out = e.out.clone();
                let session = e.session.clone();
                let clean_after = e.clean_after;
                self.enc = None;
                SendMessageW(self.ctl.progress, PBM_SETPOS, 0, 0);
                self.ui_set_idle();
                match outcome {
                    Some(Ok(EncodeOutcome::Done(s))) => {
                        let msg = format!(
                            "合成完成：{} 帧，{:.2} MB，{}x{}，{:.1} 秒\n输出：{}",
                            s.frames,
                            s.bytes as f64 / 1048576.0,
                            s.width,
                            s.height,
                            s.seconds,
                            out.display()
                        );
                        if clean_after {
                            // 规格：合成成功自动清理缓存（默认勾选）
                            if Session::delete(&session).is_ok() {
                                self.refresh_session_list();
                            }
                        }
                        message(self.hwnd, "合成完成", &msg);
                    }
                    Some(Ok(EncodeOutcome::Cancelled(s))) => {
                        message(
                            self.hwnd,
                            "已取消合成",
                            &format!(
                                "已写入 {} 帧，文件仍可播放：\n{}",
                                s.frames,
                                out.display()
                            ),
                        );
                    }
                    Some(Err(err)) => message(self.hwnd, "合成失败", &err),
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
        if !recording && self.preview_cap.is_some() {
            // 先抓完再把结果搬出来，避免 `&self.preview_cap` 的不可变借用挡住后面的字段修改
            let grabbed = self.preview_cap.as_ref().map(|c| c.grab());
            if let Some(res) = grabbed {
                match res {
                    Ok(f) => {
                        let sz = (f.width, f.height);
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
                        self.preview = Some(f);
                        InvalidateRect(self.hwnd, &self.preview_rect, 0);
                    }
                    Err(e) => {
                        // 目标被关掉/最小化时这个错误每 500ms 重复一次：
                        // 只在"错误内容变了"或"距上次记录 ≥10 秒"时写
                        let now = std::time::Instant::now();
                        let due = match &self.preview_err {
                            None => true,
                            Some((old, at)) => *old != e || now.duration_since(*at).as_secs() >= 10,
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

    /// 底部操作区随状态切换（规格：Idle 显示【开始录制】；录制中只显示提示；
    /// 合成中显示进度条与【取消合成】）。此前这三样一直可见，是明显的可用性问题。
    unsafe fn ui_set_idle(&self) {
        ShowWindow(self.ctl.btn_record, SW_SHOW);
        ShowWindow(self.ctl.progress, 0);
        ShowWindow(self.ctl.btn_cancel, 0);
        let t = win::wide("待机");
        SetWindowTextW(self.ctl.state, t.as_ptr());
    }

    unsafe fn ui_set_recording(&self) {
        ShowWindow(self.ctl.btn_record, 0);
        ShowWindow(self.ctl.progress, 0);
        ShowWindow(self.ctl.btn_cancel, 0);
        let t = win::wide("录制中…主窗口已最小化，请使用悬浮操作条（暂停 / 标记 / 停止）");
        SetWindowTextW(self.ctl.state, t.as_ptr());
    }

    unsafe fn ui_set_encoding(&self) {
        ShowWindow(self.ctl.btn_record, 0);
        ShowWindow(self.ctl.progress, SW_SHOW);
        ShowWindow(self.ctl.btn_cancel, SW_SHOW);
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
                let scale = f64::min(bw as f64 / frame.width as f64, bh as f64 / frame.height as f64);
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
                    bmiColors: [RGBQUAD { rgbBlue: 0, rgbGreen: 0, rgbRed: 0, rgbReserved: 0 }],
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
            let mut edge = r;
            DrawEdge(hdc, &mut edge, BDR_SUNKENOUTER, BF_RECT);
            // ⚠️ 必须显式选 UI 字体。窗口 DC 的默认字体是 System 位图字体，
            //    高 DPI 下会大得离谱、又被 160×100 的框裁掉（实测踩过）。
            let old = SelectObject(hdc, self.font);
            SetBkMode(hdc, 1); // TRANSPARENT
            let t = win::wide("预览区域\n拾取窗口后显示");
            // 先量出两行文字的实际高度，再垂直居中 —— 固定 top 偏移在高 DPI 下会偏上。
            // ⚠️ DT_CALCRECT 会**改写矩形**（把 right/bottom 收到文字实际大小），
            //    所以必须用单独的 measure 矩形量，否则后面的居中会按被收窄的框算，
            //    文字表现为贴着左边（实测踩过）。
            let mut measure = RECT { left: r.left + 4, top: 0, right: r.right - 4, bottom: 0 };
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

fn pack(lo: i32, hi: i32) -> isize {
    ((hi as isize) << 16) | (lo as isize & 0xffff)
}

fn default_cache_root() -> PathBuf {
    if let Ok(p) = std::env::var("CANLAPSE_CACHE") {
        return PathBuf::from(p);
    }
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(base).join("CanLapse").join("cache")
}

/// 粗略判断"画布可能没浮动"：客户区里出现明显偏窄或偏宽的比例，
/// 或者窗口类名是常见主框架。**只做提醒，不阻止用户继续**（规格要求保留强行继续的能力）。
fn looks_like_panel(info: &win::WindowInfo) -> bool {
    let (w, h) = info.client_size();
    if w <= 0 || h <= 0 {
        return false;
    }
    let ratio = w as f64 / h as f64;
    // 常见绘画软件主窗口（还没把画布拖出来）的类名特征
    let main_frame = matches!(
        info.class_name.as_str(),
        "Chrome_WidgetWin_1" | "Qt5157QWindowIcon" | "Photoshop" | "Sai2Class"
    );
    main_frame || ratio > 3.2 || ratio < 0.35
}

unsafe fn message(owner: HWND, title: &str, text: &str) {
    let t = win::wide(title);
    let b = win::wide(text);
    MessageBoxW(owner, b.as_ptr(), t.as_ptr(), MB_OK | MB_ICONINFORMATION);
}

unsafe fn confirm(owner: HWND, text: &str) -> bool {
    let t = win::wide("请确认");
    let b = win::wide(text);
    // MB_YESNO | MB_ICONQUESTION
    windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(owner, b.as_ptr(), t.as_ptr(), 4 | 0x20)
        == 6
}

/// 用资源管理器打开文件夹（规格里的「打开文件夹」）
pub fn open_folder(dir: &std::path::Path) {
    let _ = std::process::Command::new("explorer.exe")
        .arg(dir)
        .spawn();
}

const PREVIEW_CLASS: &str = "CanLapseBigPreview";

unsafe extern "system" fn preview_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let app = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            if !app.is_null() {
                if let Some(frame) = &(*app).preview {
                    let mut cr: RECT = std::mem::zeroed();
                    GetClientRect(hwnd, &mut cr);
                    let (cw, ch) = (cr.right, cr.bottom);
                    let scale = f64::min(cw as f64 / frame.width as f64, ch as f64 / frame.height as f64);
                    let dw = (frame.width as f64 * scale) as i32;
                    let dh = (frame.height as f64 * scale) as i32;
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
                        bmiColors: [RGBQUAD { rgbBlue: 0, rgbGreen: 0, rgbRed: 0, rgbReserved: 0 }],
                    };
                    StretchDIBits(
                        hdc,
                        (cw - dw) / 2,
                        (ch - dh) / 2,
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
                }
            }
            EndPaint(hwnd, &ps);
            0
        }
        WM_DESTROY => 0,
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 启动 GUI（阻塞到窗口关闭）
pub fn run(initial_target: Option<isize>) -> i32 {
    // 运行日志：debug 构建默认写 %LOCALAPPDATA%\CanLapse\canlapse.log（GUI 没有控制台，
    // 用户测试后只能靠这个文件定位问题）；release 构建需显式设 CANLAPSE_LOG 才写。
    let _ = crate::log::init();
    logline!("");
    logline!(
        "==== CanLapse {} ({}) 启动 ====",
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) { "debug" } else { "release" }
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
    // 崩溃现场落盘：GUI 是 windows 子系统，stderr 默认不可见，panic 信息会完全丢失。
    // 装了钩子后，一旦崩溃可查看 %LOCALAPPDATA%\CanLapse\crash.log（追加写入）。
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

        // 注册主窗口类与预览窗口类
        for (name, proc_) in [
            (
                MAIN_CLASS,
                wndproc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
            ),
            (PREVIEW_CLASS, preview_proc),
        ] {
            let c = cls(name);
            let brush = windows_sys::Win32::Graphics::Gdi::CreateSolidBrush(0x00F0_F0F0);
            let wc = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(proc_),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: win::module_handle(),
                hIcon: std::ptr::null_mut(),
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
        let t = win::wide("CanLapse");
        let hwnd = CreateWindowExW(
            0,
            c.as_ptr(),
            t.as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_THICKFRAME,
            100,
            80,
            DEFAULT_W,
            DEFAULT_H,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            win::module_handle(),
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return 2;
        }
        // 命令行预先指定目标（便于脚本化验证，也方便用户直接从命令行带目标启动）
        if let Some(h) = initial_target {
            if let Some(app) = app_mut(hwnd) {
                app.target = Some(win::describe(h as HWND));
                app.pick_state = PickState::Ok;
                app.preview_cap = ThumbnailCapture::new(h as HWND).ok();
                if let Some(cap) = &app.preview_cap {
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
