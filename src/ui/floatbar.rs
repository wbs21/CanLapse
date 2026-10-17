//! 录制控制条（悬浮条）：无边框 / 置顶 / **不抢焦点** / 整条可拖动。
//!
//! 外观全部自绘（不建子控件）：系统 Button 会带一圈灰色 3D 边框，在置顶浮条上非常刺眼。
//! 这里用 GDI 画深色圆角面板 + 扁平按钮 + 状态圆点，配合 Win11 的 DWM 圆角
//! （`DWMWA_WINDOW_CORNER_PREFERENCE`）与"无边框色"（`DWMWA_BORDER_COLOR`），
//! 在不引入任何第三方 UI 框架的前提下做到接近现代浮层的观感。
//!
//! 交互：
//! * 点按钮 → 暂停/继续、标记、停止；
//! * 按住按钮以外的任何位置 → 拖动整条（`HTCAPTION`）；
//! * `WS_EX_NOACTIVATE` 保证点击不把焦点从绘画软件抢走。

use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, Ellipse, EndPaint, FillRect,
    FrameRect, GetStockObject, RoundRect, ScreenToClient, SelectObject, SetBkMode, SetTextColor, DEFAULT_GUI_FONT,
    FW_NORMAL, HDC, HGDIOBJ, NULL_PEN,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetCursorPos, LoadCursorW, RegisterClassW,
    SetCursor, SetTimer, SetWindowPos, SetWindowTextW, ShowWindow,
    HTCAPTION, HWND_TOPMOST, IDC_HAND, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_SHOWNOACTIVATE,
    WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_NCHITTEST, WM_PAINT, WM_SETCURSOR, WM_TIMER, WNDCLASSW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use windows_sys::Win32::UI::Controls::WM_MOUSELEAVE;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};

use crate::recorder::{self, Shared};
use crate::win;

const BAR_CLASS: &str = "CanLapseFloatBar";
const TIMER_REFRESH: usize = 1;

const BAR_W: i32 = 440;
const BAR_H: i32 = 44;
const PAD: i32 = 8;
const BTN_W: i32 = 66;
const BTN_H: i32 = 30;
const BTN_GAP: i32 = 6;

/// 按钮索引
const BTN_PAUSE: usize = 0;
const BTN_MARK: usize = 1;
const BTN_STOP: usize = 2;
const NONE: usize = 99;

/// 悬浮条是进程内单例，共享状态放在静态里，避免往 GWLP_USERDATA 塞 Arc 裸指针。
static BAR_SHARED: Mutex<Option<Arc<Shared>>> = Mutex::new(None);
/// 悬停 / 按下的按钮索引
static HOVER: AtomicUsize = AtomicUsize::new(NONE);
static PRESSED: AtomicUsize = AtomicUsize::new(NONE);
/// 跟踪鼠标离开（没有它，鼠标移出后按钮会一直保持悬停高亮）
static TRACKING: AtomicIsize = AtomicIsize::new(0);
static FONT: OnceLock<isize> = OnceLock::new();
/// 定时器 tick 计数（红点脉冲用）
static TICK: AtomicUsize = AtomicUsize::new(0);

// ---- 配色（COLORREF 是 0x00BBGGRR）----
const C_BG: u32 = 0x0036_2F2B; // #2B2F36
const C_BG_TOP: u32 = 0x0047_3F3A; // #3A3F47 顶部高光线
const C_BORDER: u32 = 0x004B_423C; // #3C424B
const C_BTN_HOVER: u32 = 0x0048_403A; // #3A4048
const C_BTN_DOWN: u32 = 0x005A_4F47; // #474F5A
const C_TEXT: u32 = 0x00EE_E9E6; // #E6E9EE
const C_MUTED: u32 = 0x00AE_A39A; // #9AA3AE
const C_REC: u32 = 0x0052_52E0; // #E05252 红点
const C_REC_DIM: u32 = 0x0038_3AA8; // #A83A38 红点脉冲暗色
const C_DIVIDER: u32 = 0x0048_3F3A; // 分隔线
const C_PAUSE: u32 = 0x0044_B5F2; // #F2B544 琥珀
const C_STOP: u32 = 0x008A_8AF0; // #F08A8A 停止按钮文字

/// 按钮客户区矩形（3 个）
fn button_rects() -> [(i32, i32, i32, i32); 3] {
    let y = (BAR_H - BTN_H) / 2;
    let mut out = [(0, 0, 0, 0); 3];
    for (i, slot) in out.iter_mut().enumerate() {
        let x = PAD + 18 + (BTN_W + BTN_GAP) * i as i32; // 18 = 左侧状态圆点占位
        *slot = (x, y, BTN_W, BTN_H);
    }
    out
}

fn hit_button(px: i32, py: i32) -> usize {
    for (i, (x, y, w, h)) in button_rects().iter().enumerate() {
        if px >= *x && px < *x + *w && py >= *y && py < *y + *h {
            return i;
        }
    }
    NONE
}

unsafe fn cursor_client(hwnd: HWND) -> (i32, i32) {
    let mut p: POINT = std::mem::zeroed();
    GetCursorPos(&mut p);
    ScreenToClient(hwnd, &mut p);
    (p.x, p.y)
}

/// 小字号 UI 字体（只创建一次，进程生命周期内复用；不删句柄，量很小）
unsafe fn ui_font() -> HGDIOBJ {
    let h = *FONT.get_or_init(|| {
        let face = win::wide("Microsoft YaHei UI");
        let mut f = CreateFontW(
            -15, // 字高（像素）
            0,
            0,
            0,
            FW_NORMAL as i32,
            0,
            0,
            0,
            1, // DEFAULT_CHARSET
            0,
            0,
            0,
            0,
            face.as_ptr(),
        ) as isize;
        if f == 0 {
            let seg = win::wide("Segoe UI");
            f = CreateFontW(-15, 0, 0, 0, FW_NORMAL as i32, 0, 0, 0, 1, 0, 0, 0, 0, seg.as_ptr())
                as isize;
        }
        if f == 0 {
            f = GetStockObject(DEFAULT_GUI_FONT) as isize;
        }
        f
    });
    h as HGDIOBJ
}

unsafe fn bar_font() -> HGDIOBJ {
    // 半粗体给按钮，普通字体给信息行；这里图省事用同一个（YaHei UI 15px）
    ui_font()
}

fn color(v: u32) -> u32 {
    v
}

/// 选刷、选空笔后画填充（GDI 的 RoundRect/Ellipse 用的是 DC 当前笔刷，不是参数里的）
unsafe fn fill_round_rect(dc: HDC, r: RECT, brush: HGDIOBJ) {
    let ob = SelectObject(dc, brush);
    let op = SelectObject(dc, GetStockObject(NULL_PEN));
    RoundRect(dc, r.left, r.top, r.right, r.bottom, 16, 16);
    SelectObject(dc, op);
    SelectObject(dc, ob);
}

unsafe fn fill_ellipse(dc: HDC, r: RECT, brush: HGDIOBJ) {
    let ob = SelectObject(dc, brush);
    let op = SelectObject(dc, GetStockObject(NULL_PEN));
    Ellipse(dc, r.left, r.top, r.right, r.bottom);
    SelectObject(dc, op);
    SelectObject(dc, ob);
}

unsafe fn text(dc: HDC, s: &str, rc: RECT, col: u32, align: u32) {
    SetTextColor(dc, col);
    let t = win::wide(s);
    let mut r = rc;
    DrawTextW(
        dc,
        t.as_ptr(),
        -1,
        &mut r,
        align | 0x0020, // DT_SINGLELINE
    );
}

unsafe fn paint(hwnd: HWND) {
    let mut ps: windows_sys::Win32::Graphics::Gdi::PAINTSTRUCT = std::mem::zeroed();
    let dc = BeginPaint(hwnd, &mut ps);
    let mut cr: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut cr);
    let (w, h) = (cr.right, cr.bottom);

    // 背景 + 顶部高光线
    let bg = CreateSolidBrush(color(C_BG));
    FillRect(dc, &cr, bg);
    let top = RECT {
        left: 0,
        top: 0,
        right: w,
        bottom: 1,
    };
    let br = CreateSolidBrush(color(C_BG_TOP));
    FillRect(dc, &top, br);

    // 1px 内边框（比系统灰边淡得多）
    let border = CreateSolidBrush(color(C_BORDER));
    FrameRect(dc, &cr, border);

    // 文字基线设置
    let old_font = SelectObject(dc, bar_font());
    SetBkMode(dc, 1); // TRANSPARENT

    // 左侧状态圆点：录制=红、暂停/前台他窗=琥珀
    let (Ok(guard), true) = (BAR_SHARED.lock(), true) else {
        EndPaint(hwnd, &ps);
        return;
    };
    let Some(shared) = guard.as_ref() else {
        EndPaint(hwnd, &ps);
        return;
    };
    let paused = shared.is_paused() || shared.fg_paused();
    // 录制中让红点"呼吸"（定时器 200ms 触发一次重绘，明暗交替约 0.4s）
    let pulse = TICK.load(Ordering::Relaxed) % 2 == 0;
    let dot_color = if paused {
        C_PAUSE
    } else if pulse {
        C_REC
    } else {
        C_REC_DIM
    };
    let dot = CreateSolidBrush(color(dot_color));
    let (dx, dy) = (PAD + 4, h / 2 - 4);
    let dot_rc = RECT {
        left: dx,
        top: dy,
        right: dx + 8,
        bottom: dy + 8,
    };
    fill_ellipse(dc, dot_rc, dot);

    // 三个扁平按钮（悬停/按下才画底，平时完全透明，避免"灰边"观感）
    let labels = [
        if shared.is_paused() { "继续" } else { "暂停" },
        "标记",
        "停止",
    ];
    let hover = HOVER.load(Ordering::Relaxed);
    let pressed = PRESSED.load(Ordering::Relaxed);
    for (i, (x, y, bw, bh)) in button_rects().iter().enumerate() {
        if pressed == i || hover == i {
            let brush = CreateSolidBrush(color(if pressed == i {
                C_BTN_DOWN
            } else {
                C_BTN_HOVER
            }));
            let rc = RECT {
                left: *x,
                top: *y,
                right: *x + *bw,
                bottom: *y + *bh,
            };
            fill_round_rect(dc, rc, brush);
            DeleteObject(brush as _);
        }
        let rc = RECT {
            left: *x,
            top: *y,
            right: *x + *bw,
            bottom: *y + *bh,
        };
        let col = if i == BTN_STOP { C_STOP } else { C_TEXT };
        text(dc, labels[i], rc, color(col), 0x0000_0001 | 0x0000_0004); // DT_CENTER | DT_VCENTER
    }

    // 右侧信息区：先画一条 1px 分隔线，让"控件"和"状态"分开
    let info_x = PAD + 18 + (BTN_W + BTN_GAP) * 3 + 12;
    let dv_rc = RECT {
        left: info_x - 8,
        top: 12,
        right: info_x - 7,
        bottom: h - 12,
    };
    let dv = CreateSolidBrush(color(C_DIVIDER));
    FillRect(dc, &dv_rc, dv);
    DeleteObject(dv as _);

    // 信息文案："00:12 · 3 帧"（暂停时补一句原因，缩短到能放下）
    let mut info = format!(
        "{} · {} 帧",
        recorder::format_duration(shared.elapsed_ms()),
        shared.frames()
    );
    if shared.markers() > 0 {
        info.push_str(&format!(" · {} 标记", shared.markers()));
    }
    if shared.fg_paused() {
        info = format!("{} · 回到画布继续", recorder::format_duration(shared.elapsed_ms()));
    } else if shared.is_paused() {
        info.push_str(" · 已暂停");
    }
    let rc = RECT {
        left: info_x,
        top: 0,
        right: w - PAD,
        bottom: h,
    };
    text(dc, &info, rc, color(C_MUTED), 0x0000_0000 | 0x0000_0004); // DT_LEFT | DT_VCENTER

    SelectObject(dc, old_font);
    DeleteObject(bg as _);
    DeleteObject(br as _);
    DeleteObject(border as _);
    DeleteObject(dot as _);
    EndPaint(hwnd, &ps);
}

unsafe extern "system" fn bar_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    win::guard(|| bar_proc_inner(hwnd, msg, wparam, lparam))
}

unsafe fn bar_proc_inner(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let x = (lparam as i32) & 0xffff;
    let y = ((lparam as i32) >> 16) & 0xffff;
    match msg {
        WM_PAINT => {
            paint(hwnd);
            0
        }
        WM_ERASEBKGND => 1,
        WM_NCHITTEST => {
            // ⚠️ WM_NCHITTEST 的 lparam 是**屏幕坐标**，必须先转客户区
            let (cx, cy) = cursor_client(hwnd);
            // 按钮区域让窗口自己处理（否则按不动），其余位置整条可拖
            if hit_button(cx, cy) != NONE {
                1 // HTCLIENT
            } else {
                HTCAPTION as LRESULT
            }
        }
        WM_SETCURSOR => {
            // ⚠️ WM_SETCURSOR 的 lparam 是 (hit code | 消息号)，**不是坐标**
            let (cx, cy) = cursor_client(hwnd);
            let over = hit_button(cx, cy) != NONE;
            if over {
                let h = LoadCursorW(std::ptr::null_mut(), IDC_HAND);
                SetCursor(h);
                1
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        WM_MOUSEMOVE => {
            let idx = hit_button(x, y);
            if HOVER.swap(idx, Ordering::Relaxed) != idx {
                invalidate(hwnd);
            }
            if TRACKING.swap(1, Ordering::Relaxed) == 0 {
                let mut tme: TRACKMOUSEEVENT = std::mem::zeroed();
                tme.cbSize = std::mem::size_of::<TRACKMOUSEEVENT>() as u32;
                tme.dwFlags = TME_LEAVE;
                tme.hwndTrack = hwnd;
                TrackMouseEvent(&mut tme);
            }
            0
        }
        WM_MOUSELEAVE => {
            TRACKING.store(0, Ordering::Relaxed);
            HOVER.store(NONE, Ordering::Relaxed);
            invalidate(hwnd);
            0
        }
        WM_LBUTTONDOWN => {
            let idx = hit_button(x, y);
            PRESSED.store(idx, Ordering::Relaxed);
            invalidate(hwnd);
            0
        }
        WM_LBUTTONUP => {
            let idx = hit_button(x, y);
            let pressed = PRESSED.swap(NONE, Ordering::Relaxed);
            invalidate(hwnd);
            if idx != NONE && idx == pressed {
                if let Ok(guard) = BAR_SHARED.lock() {
                    if let Some(shared) = guard.as_ref() {
                        match idx {
                            BTN_PAUSE => {
                                shared.toggle_pause();
                            }
                            BTN_MARK => shared.request_mark(),
                            BTN_STOP => shared.request_stop(),
                            _ => {}
                        }
                    }
                }
            }
            0
        }
        WM_TIMER => {
            if wparam == TIMER_REFRESH {
                TICK.fetch_add(1, Ordering::Relaxed);
                // 状态全在 WM_PAINT 里现取，这里只需要触发重绘 + 更新窗口标题（便于自动化识别）
                update_title(hwnd);
                invalidate(hwnd);
            }
            0
        }
        WM_DESTROY => 0,
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn invalidate(hwnd: HWND) {
    windows_sys::Win32::Graphics::Gdi::InvalidateRect(hwnd, std::ptr::null(), 0);
}

/// 把状态摘要写进窗口标题（无边框窗口看不到标题，但自动化/排查时有用）
unsafe fn update_title(hwnd: HWND) {
    let Ok(guard) = BAR_SHARED.lock() else { return };
    let Some(shared) = guard.as_ref() else { return };
    let t = win::wide(&format!(
        "{} · {} 帧{}",
        recorder::format_duration(shared.elapsed_ms()),
        shared.frames(),
        if shared.is_paused() { " · 已暂停" } else { "" }
    ));
    SetWindowTextW(hwnd, t.as_ptr());
}

unsafe fn ensure_class() {
    let cls = win::wide(BAR_CLASS);
    let wc = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(bar_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: win::module_handle(),
        hIcon: std::ptr::null_mut(),
        hCursor: std::ptr::null_mut(),
        hbrBackground: std::ptr::null_mut(), // 自绘，不用类背景刷
        lpszMenuName: std::ptr::null(),
        lpszClassName: cls.as_ptr(),
    };
    RegisterClassW(&wc);
}

/// 让 Win11 给窗口画圆角、去掉系统窗口边框色；老系统上调用失败无副作用
unsafe fn polish_frame(hwnd: HWND) {
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
    // DWMWA_WINDOW_CORNER_PREFERENCE = 33, DWMWCP_ROUND = 2
    let pref: u32 = 2;
    let _ = DwmSetWindowAttribute(
        hwnd,
        33,
        &pref as *const u32 as *const _,
        std::mem::size_of::<u32>() as u32,
    );
    // DWMWA_BORDER_COLOR = 34, DWMWA_COLOR_NONE = 0xFFFFFFFE（去掉那圈灰边）
    let none: u32 = 0xFFFF_FFFE;
    let _ = DwmSetWindowAttribute(
        hwnd,
        34,
        &none as *const u32 as *const _,
        std::mem::size_of::<u32>() as u32,
    );
}

/// 创建录制控制条（默认放在屏幕左上偏下位置）
pub fn create(shared: Arc<Shared>) -> Option<HWND> {
    if let Ok(mut g) = BAR_SHARED.lock() {
        *g = Some(shared);
    }
    HOVER.store(NONE, Ordering::Relaxed);
    PRESSED.store(NONE, Ordering::Relaxed);
    unsafe {
        ensure_class();
        let cls = win::wide(BAR_CLASS);
        let title = win::wide("CanLapse");
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            cls.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            40,
            120,
            BAR_W,
            BAR_H,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            win::module_handle(),
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return None;
        }
        polish_frame(hwnd);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
        SetTimer(hwnd, TIMER_REFRESH, 200, None);
        update_title(hwnd);
        invalidate(hwnd);
        Some(hwnd)
    }
}

pub fn destroy(hwnd: HWND) {
    if !hwnd.is_null() {
        unsafe {
            DestroyWindow(hwnd);
        }
    }
    if let Ok(mut g) = BAR_SHARED.lock() {
        *g = None;
    }
}

/// 悬浮条矩形（用于落点计算，避免挡住画布）
pub fn rect_of(hwnd: HWND) -> Option<RECT> {
    if hwnd.is_null() {
        return None;
    }
    let mut rc: RECT = unsafe { std::mem::zeroed() };
    let ok = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rc) };
    if ok == 0 {
        None
    } else {
        Some(rc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 按钮命中区域与布局一致() {
        let rects = button_rects();
        assert_eq!(rects.len(), 3);
        // 每个按钮中心命中自己
        for (i, (x, y, w, h)) in rects.iter().enumerate() {
            assert_eq!(hit_button(x + w / 2, y + h / 2), i, "按钮 {i} 中心未命中");
        }
        // 左端（圆点区域）、按钮间隙、右侧信息区都不应命中按钮
        assert_eq!(hit_button(2, BAR_H / 2), NONE);
        assert_eq!(hit_button(rects[0].0 - 2, BAR_H / 2), NONE);
        assert_eq!(hit_button(BAR_W - 4, BAR_H / 2), NONE);
        // 按钮不能重叠
        for w in rects.windows(2) {
            assert!(w[0].0 + w[0].2 <= w[1].0, "按钮重叠");
        }
    }
}