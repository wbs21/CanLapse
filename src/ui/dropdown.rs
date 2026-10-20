//! 自绘下拉选择（替代系统 ComboBox）。
//!
//! 系统 ComboBox / Edit 在不吃 `DarkMode_Explorer` 的环境里会保持白底，和深色主界面割裂；
//! 这里用一个自绘的"字段 + 弹出列表"实现，外观与录制控制条一致：
//! 圆角深色字段、右侧小箭头、弹出列表悬停高亮、选中项用主题绿。
//!
//! 对外只暴露 `create / set_items / set_sel / get_sel / close_open`，选中变化时向父窗口发
//! `WM_COMMAND`（id + CBN_SELCHANGE），和原来的 ComboBox 行为一致。

use std::collections::HashMap;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreatePen, DeleteObject, EndPaint, GetStockObject, InvalidateRect, Polyline,
    SelectObject, SetBkMode, NULL_BRUSH, PS_SOLID,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetDlgCtrlID, GetParent,
    GetWindowLongPtrW, GetWindowRect, PostMessageW, RegisterClassW, SendMessageW,
    SetWindowLongPtrW, ShowWindow, GWLP_USERDATA, SW_SHOWNOACTIVATE, WM_COMMAND, WM_DESTROY,
    WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_SETFONT, WNDCLASSW,
    WS_CHILD, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use windows_sys::Win32::UI::Controls::WM_MOUSELEAVE;

use crate::ui::theme;
use crate::win;

const FIELD_CLASS: &str = "CanLapseDropdown";
const POPUP_CLASS: &str = "CanLapseDropdownPopup";
const ROW_H: i32 = 26;
const PAD: i32 = 8;

struct FieldData {
    items: Vec<String>,
    sel: usize,
    hover: bool,
}

struct PopupData {
    items: Vec<String>,
    sel: usize,
    hover_row: isize,
    field: HWND,
}

static FIELDS: Mutex<Option<HashMap<isize, FieldData>>> = Mutex::new(None);
/// 当前打开的弹层（进程内同时只允许一个）
static OPEN: AtomicIsize = AtomicIsize::new(0);

fn with_field<R>(hwnd: HWND, f: impl FnOnce(&mut FieldData) -> R) -> Option<R> {
    let mut g = FIELDS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let map = g.get_or_insert_with(HashMap::new);
    let st = map.get_mut(&(hwnd as isize))?;
    Some(f(st))
}

unsafe fn register_class(
    name: &str,
    proc_: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
) {
    let c = win::wide(name);
    let wc = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(proc_),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: win::module_handle(),
        hIcon: std::ptr::null_mut(),
        hCursor: std::ptr::null_mut(),
        hbrBackground: std::ptr::null_mut(),
        lpszMenuName: std::ptr::null(),
        lpszClassName: c.as_ptr(),
    };
    RegisterClassW(&wc);
}

/// 创建下拉字段（父窗口子控件）
pub unsafe fn create(parent: HWND, id: usize, x: i32, y: i32, w: i32, h: i32) -> HWND {
    register_class(FIELD_CLASS, field_proc);
    register_class(POPUP_CLASS, popup_proc);
    let c = win::wide(FIELD_CLASS);
    let t = win::wide("");
    let hwnd = CreateWindowExW(
        0,
        c.as_ptr(),
        t.as_ptr(),
        WS_CHILD | WS_VISIBLE,
        x,
        y,
        w,
        h,
        parent,
        id as _,
        win::module_handle(),
        std::ptr::null(),
    );
    if hwnd.is_null() {
        return hwnd;
    }
    if let Ok(mut g) = FIELDS.lock() {
        let map = g.get_or_insert_with(HashMap::new);
        map.insert(
            hwnd as isize,
            FieldData {
                items: Vec::new(),
                sel: 0,
                hover: false,
            },
        );
    }
    let f = theme::font();
    SendMessageW(hwnd, WM_SETFONT, f as WPARAM, 1);
    hwnd
}

pub unsafe fn set_items(hwnd: HWND, items: &[&str]) {
    with_field(hwnd, |st| {
        st.items = items.iter().map(|s| s.to_string()).collect();
        if st.sel >= st.items.len() {
            st.sel = 0;
        }
    });
    InvalidateRect(hwnd, std::ptr::null(), 0);
}

pub unsafe fn set_sel(hwnd: HWND, idx: usize) {
    with_field(hwnd, |st| {
        st.sel = idx.min(st.items.len().saturating_sub(1))
    });
    InvalidateRect(hwnd, std::ptr::null(), 0);
}

pub unsafe fn get_sel(hwnd: HWND) -> usize {
    with_field(hwnd, |st| st.sel).unwrap_or(0)
}

/// 关闭当前打开的弹层（主窗口重排/录制开始时调用）
pub unsafe fn close_open() {
    let p = OPEN.swap(0, Ordering::SeqCst);
    if p != 0 {
        ReleaseCapture();
        DestroyWindow(p as HWND);
    }
}

// ---------------------------------------------------------------------------
// 字段绘制
// ---------------------------------------------------------------------------

unsafe fn paint_field(hwnd: HWND) {
    let mut ps: windows_sys::Win32::Graphics::Gdi::PAINTSTRUCT = std::mem::zeroed();
    let dc = BeginPaint(hwnd, &mut ps);
    let mut rc: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut rc);
    let (w, h) = (rc.right, rc.bottom);

    let (text, hover) = with_field(hwnd, |st| {
        (st.items.get(st.sel).cloned().unwrap_or_default(), st.hover)
    })
    .unwrap_or_default();
    let open = OPEN.load(Ordering::SeqCst) != 0;

    let bg = if open || hover {
        theme::HOVER
    } else {
        theme::PANEL
    };
    theme::fill_round(dc, rc, bg, 10);
    let pen = CreatePen(PS_SOLID, 1, theme::BORDER);
    let ob = SelectObject(dc, GetStockObject(NULL_BRUSH));
    let op = SelectObject(dc, pen);
    windows_sys::Win32::Graphics::Gdi::RoundRect(
        dc,
        rc.left,
        rc.top,
        rc.right - 1,
        rc.bottom - 1,
        10,
        10,
    );
    SelectObject(dc, op);
    SelectObject(dc, ob);
    DeleteObject(pen);

    let old = SelectObject(dc, theme::font());
    SetBkMode(dc, 1);
    let text_rc = RECT {
        left: PAD,
        top: 0,
        right: w - 24,
        bottom: h,
    };
    theme::text(dc, &text, text_rc, theme::TEXT, 0x0000_0004);

    // 右侧小箭头
    let cx = w - 14;
    let cy = h / 2;
    let pen2 = CreatePen(PS_SOLID, 1, theme::MUTED);
    let op2 = SelectObject(dc, pen2);
    let pts = [
        POINT {
            x: cx - 4,
            y: cy - 2,
        },
        POINT { x: cx, y: cy + 2 },
        POINT {
            x: cx + 4,
            y: cy - 2,
        },
    ];
    Polyline(dc, pts.as_ptr(), 3);
    SelectObject(dc, op2);
    DeleteObject(pen2);
    SelectObject(dc, old);
    EndPaint(hwnd, &ps);
}

unsafe extern "system" fn field_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    win::guard(|| match msg {
        WM_PAINT => {
            paint_field(hwnd);
            0
        }
        WM_ERASEBKGND => 1,
        WM_MOUSEMOVE => {
            let changed = with_field(hwnd, |st| {
                let c = !st.hover;
                st.hover = true;
                c
            })
            .unwrap_or(false);
            if changed {
                InvalidateRect(hwnd, std::ptr::null(), 0);
            }
            let mut tme: TRACKMOUSEEVENT = std::mem::zeroed();
            tme.cbSize = std::mem::size_of::<TRACKMOUSEEVENT>() as u32;
            tme.dwFlags = TME_LEAVE;
            tme.hwndTrack = hwnd;
            TrackMouseEvent(&mut tme);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_MOUSELEAVE => {
            with_field(hwnd, |st| st.hover = false);
            InvalidateRect(hwnd, std::ptr::null(), 0);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_LBUTTONDOWN => {
            if OPEN.load(Ordering::SeqCst) != 0 {
                close_open();
            } else {
                open_popup(hwnd);
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    })
}

// ---------------------------------------------------------------------------
// 弹出列表
// ---------------------------------------------------------------------------

unsafe fn open_popup(field: HWND) {
    let Some((items, sel)) = with_field(field, |st| (st.items.clone(), st.sel)) else {
        return;
    };
    if items.is_empty() {
        return;
    }
    let mut wr: RECT = std::mem::zeroed();
    GetWindowRect(field, &mut wr);
    let w = wr.right - wr.left;
    let h = items.len() as i32 * ROW_H + 8;
    let c = win::wide(POPUP_CLASS);
    let t = win::wide("");
    let popup = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        c.as_ptr(),
        t.as_ptr(),
        WS_POPUP,
        wr.left,
        wr.bottom + 2,
        w,
        h,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        win::module_handle(),
        std::ptr::null(),
    );
    if popup.is_null() {
        return;
    }
    let data = Box::new(PopupData {
        items,
        sel,
        hover_row: sel as isize,
        field,
    });
    SetWindowLongPtrW(popup, GWLP_USERDATA, Box::into_raw(data) as isize);
    OPEN.store(popup as isize, Ordering::SeqCst);
    ShowWindow(popup, SW_SHOWNOACTIVATE);
    SetCapture(popup);
    InvalidateRect(popup, std::ptr::null(), 0);
    InvalidateRect(field, std::ptr::null(), 0);
}

unsafe fn popup_data(hwnd: HWND) -> Option<*mut PopupData> {
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PopupData;
    if p.is_null() {
        None
    } else {
        Some(p)
    }
}

unsafe fn paint_popup(hwnd: HWND) {
    let Some(data) = popup_data(hwnd) else { return };
    let mut ps: windows_sys::Win32::Graphics::Gdi::PAINTSTRUCT = std::mem::zeroed();
    let dc = BeginPaint(hwnd, &mut ps);
    let mut rc: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut rc);
    let w = rc.right;

    theme::fill_round(dc, rc, theme::BG, 10);
    let pen = CreatePen(PS_SOLID, 1, theme::BORDER);
    let ob = SelectObject(dc, GetStockObject(NULL_BRUSH));
    let op = SelectObject(dc, pen);
    windows_sys::Win32::Graphics::Gdi::RoundRect(
        dc,
        rc.left,
        rc.top,
        rc.right - 1,
        rc.bottom - 1,
        10,
        10,
    );
    SelectObject(dc, op);
    SelectObject(dc, ob);
    DeleteObject(pen);

    let old = SelectObject(dc, theme::font());
    SetBkMode(dc, 1);
    let d = &*data;
    for (i, item) in d.items.iter().enumerate() {
        let top = 4 + i as i32 * ROW_H;
        let row = RECT {
            left: 4,
            top,
            right: w - 4,
            bottom: top + ROW_H,
        };
        if d.hover_row == i as isize {
            theme::fill_round(dc, row, theme::HOVER, 8);
        }
        let color = if d.sel == i {
            theme::ACCENT
        } else {
            theme::TEXT
        };
        let tr = RECT {
            left: 12,
            top,
            right: w - 8,
            bottom: top + ROW_H,
        };
        theme::text(dc, item, tr, color, 0x0000_0004);
    }
    SelectObject(dc, old);
    EndPaint(hwnd, &ps);
}

unsafe extern "system" fn popup_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    win::guard(|| {
        let x = (lparam as i32) & 0xffff;
        let y = ((lparam as i32) >> 16) & 0xffff;
        match msg {
            WM_PAINT => {
                paint_popup(hwnd);
                0
            }
            WM_ERASEBKGND => 1,
            WM_MOUSEMOVE => {
                if let Some(data) = popup_data(hwnd) {
                    let row = ((y - 4) / ROW_H) as isize;
                    let d = &mut *data;
                    if row != d.hover_row {
                        d.hover_row = row;
                        InvalidateRect(hwnd, std::ptr::null(), 0);
                    }
                }
                0
            }
            WM_LBUTTONUP => {
                let mut sel: Option<usize> = None;
                let mut field: HWND = std::ptr::null_mut();
                if let Some(data) = popup_data(hwnd) {
                    let d = &*data;
                    let row = (y - 4) / ROW_H;
                    if row >= 0 && (row as usize) < d.items.len() {
                        sel = Some(row as usize);
                    }
                    field = d.field;
                }
                let p = OPEN.swap(0, Ordering::SeqCst);
                ReleaseCapture();
                if p != 0 {
                    DestroyWindow(p as HWND);
                }
                if let Some(idx) = sel {
                    if !field.is_null() {
                        set_sel(field, idx);
                        let id = GetDlgCtrlID(field) as usize;
                        let parent = GetParent(field);
                        if !parent.is_null() {
                            // CBN_SELCHANGE = 1
                            PostMessageW(parent, WM_COMMAND, id | (1 << 16), field as isize);
                        }
                    }
                }
                0
            }
            WM_LBUTTONDOWN => {
                let mut cr: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut cr);
                if x < 0 || y < 0 || x >= cr.right || y >= cr.bottom {
                    let p = OPEN.swap(0, Ordering::SeqCst);
                    ReleaseCapture();
                    if p != 0 {
                        DestroyWindow(p as HWND);
                    }
                }
                0
            }
            // 鼠标捕获被别的窗口抢走时弹层已经失去交互意义，直接关闭，
            // 避免留下一个挡住界面的 topmost 窗口。
            0x0215 => {
                let p = OPEN.swap(0, Ordering::SeqCst);
                if p == hwnd as isize {
                    DestroyWindow(hwnd);
                }
                0
            }
            WM_DESTROY => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PopupData;
                if !ptr.is_null() {
                    drop(Box::from_raw(ptr));
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                }
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    })
}
