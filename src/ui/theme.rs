//! 深色主题：配色、字体、自绘按钮/复选框/进度条，以及"让系统控件也变深色"。
//!
//! 约束是**零第三方依赖、纯 Win32**，所以：
//! * 按钮/复选框/进度条不走系统主题绘制，而是 `SetWindowSubclass` 之后自己画（圆角、悬停、
//!   按下、禁用都在这里处理）；点击仍交给控件默认过程，`WM_COMMAND` 语义不变；
//! * 下拉框与输入框没法自己画，用 uxtheme 的**未公开序数**（135 = SetPreferredAppMode，
//!   133 = AllowDarkModeForWindow）+ `SetWindowTheme(hwnd, "DarkMode_Explorer", NULL)`；
//!   老系统上调用失败只会保持浅色，不会崩；
//! * 字体统一用 Microsoft YaHei UI 15px（与录制控制条一致）。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint,
    FillRect, GetStockObject, InvalidateRect, RoundRect, SelectObject, SetBkColor, SetBkMode,
    SetTextColor, DEFAULT_GUI_FONT, FW_NORMAL, HBRUSH, HDC, HGDIOBJ, NULL_BRUSH, PS_SOLID,
};
use windows_sys::Win32::UI::Controls::SetWindowTheme;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    IsWindowEnabled, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClientRect, WM_ENABLE, WM_ERASEBKGND, WM_GETTEXT, WM_GETTEXTLENGTH, WM_KILLFOCUS,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_SETFOCUS,
};

use crate::win;

// ---- 配色（COLORREF = 0x00BBGGRR）----
pub const BG: u32 = 0x0036_2F2B; // #2B2F36 主背景
pub const PANEL: u32 = 0x0032_2E2A; // #2A2E32 分区底
pub const TEXT: u32 = 0x00EE_E9E6; // #E6E9EE 主文字
pub const MUTED: u32 = 0x00AE_A39A; // #9AA3AE 次要文字
pub const BORDER: u32 = 0x004B_423C; // #3C424B 边框/分隔线
pub const HOVER: u32 = 0x0048_403A; // #3A4048 悬停底
pub const DOWN: u32 = 0x005A_4F47; // #474F5A 按下底
pub const ACCENT: u32 = 0x0046_9C3E; // #3E9C46 主操作（开始录制）
pub const ACCENT_HOVER: u32 = 0x0050_B04A; // #4AB050
pub const ACCENT_DOWN: u32 = 0x003C_8A34; // #348A3C
pub const DANGER: u32 = 0x0052_52E0; // #E05252 停止/危险
pub const DISABLED: u32 = 0x0080_7670; // 禁用文字

const WM_MOUSELEAVE_CTRL: u32 = 0x02A3;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// 次要按钮：平时透明，悬停/按下才出现底色
    Ghost,
    /// 主操作：绿底白字
    Accent,
    /// 危险操作：悬停时红字
    Danger,
    /// 复选框
    Check,
    /// 进度条
    Progress,
}

#[derive(Default, Clone, Copy)]
struct State {
    hover: bool,
    pressed: bool,
}

static STATES: Mutex<Option<HashMap<isize, State>>> = Mutex::new(None);
static BG_BRUSH: OnceLock<isize> = OnceLock::new();
/// 字体按“物理像素高度”缓存。DPI 切换后同一个逻辑字号会得到不同高度，
/// 因此不能只用 OnceLock 缓存一个字体。
static FONTS: OnceLock<Mutex<HashMap<i32, isize>>> = OnceLock::new();

fn with_state<R>(f: impl FnOnce(&mut HashMap<isize, State>) -> R) -> R {
    let mut guard = STATES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    f(map)
}

fn font_for(base_px: i32) -> HGDIOBJ {
    let height = super::scale::px(base_px).max(1);
    let cache = FONTS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = cache.lock() {
        if let Some(v) = map.get(&height) {
            return *v as HGDIOBJ;
        }
        let face = win::wide(crate::lang::ui_font_face());
        let mut f = unsafe {
            CreateFontW(
                -height,
                0,
                0,
                0,
                FW_NORMAL as i32,
                0,
                0,
                0,
                1,
                0,
                0,
                0,
                0,
                face.as_ptr(),
            ) as isize
        };
        if f == 0 {
            f = unsafe { GetStockObject(DEFAULT_GUI_FONT) as isize };
        }
        map.insert(height, f);
        return f as HGDIOBJ;
    }
    unsafe { GetStockObject(DEFAULT_GUI_FONT) as HGDIOBJ }
}

pub fn font() -> HGDIOBJ {
    font_for(11)
}

/// 主背景画刷（窗口类背景 + `WM_CTLCOLORSTATIC` 用）
pub fn bg_brush() -> HBRUSH {
    let b = *BG_BRUSH.get_or_init(|| unsafe { CreateSolidBrush(BG) as isize });
    b as HBRUSH
}

/// 15px 标题字体：合成遮罩的"视频合成中"用
pub fn heading_font() -> HGDIOBJ {
    font_for(15)
}

/// 9px 小字体：窄按钮放不下时自动降一档
pub fn small_font() -> HGDIOBJ {
    font_for(9)
}

unsafe fn text_width(dc: HDC, s: &str) -> i32 {
    use windows_sys::Win32::Foundation::SIZE;
    let t = win::wide(s);
    let mut sz: SIZE = std::mem::zeroed();
    windows_sys::Win32::Graphics::Gdi::GetTextExtentPoint32W(
        dc,
        t.as_ptr(),
        t.len().saturating_sub(1) as i32,
        &mut sz,
    );
    sz.cx
}

/// 画控件文字：放不下时自动换小一号字体，避免被裁成空白
unsafe fn control_label(dc: HDC, hwnd: HWND, rc: RECT, color: u32, align: u32, max_w: i32) {
    let s = control_text(hwnd);
    let tw = text_width(dc, &s);
    let f = if tw > max_w { small_font() } else { font() };
    let old = SelectObject(dc, f);
    text(dc, &s, rc, color, align);
    SelectObject(dc, old);
}

unsafe fn solid(color: u32) -> HGDIOBJ {
    CreateSolidBrush(color) as HGDIOBJ
}

pub unsafe fn text(dc: HDC, s: &str, rc: RECT, color: u32, align: u32) {
    SetTextColor(dc, color);
    let t = win::wide(s);
    let mut r = rc;
    DrawTextW(dc, t.as_ptr(), -1, &mut r, align | 0x0020); // DT_SINGLELINE
}

/// 多行文字。`text()` 会强制 `DT_SINGLELINE`，适合按钮和标签；合成结果里的
/// “统计信息 + 输出路径”必须用这个版本，否则换行后的路径会被整行吞掉。
pub unsafe fn text_block(dc: HDC, s: &str, rc: RECT, color: u32, align: u32) {
    SetTextColor(dc, color);
    let t = win::wide(s);
    let mut r = rc;
    DrawTextW(dc, t.as_ptr(), -1, &mut r, align);
}

pub unsafe fn fill_round(dc: HDC, rc: RECT, color: u32, radius: i32) {
    let br = solid(color);
    let pen = CreatePen(PS_SOLID, 1, color);
    let ob = SelectObject(dc, br);
    let op = SelectObject(dc, pen);
    RoundRect(dc, rc.left, rc.top, rc.right, rc.bottom, radius, radius);
    SelectObject(dc, op);
    SelectObject(dc, ob);
    DeleteObject(pen);
    DeleteObject(br);
}

fn control_text(hwnd: HWND) -> String {
    unsafe {
        let n =
            windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW(hwnd, WM_GETTEXTLENGTH, 0, 0)
                as usize;
        let mut buf = vec![0u16; n + 1];
        let got = windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW(
            hwnd,
            WM_GETTEXT,
            buf.len(),
            buf.as_mut_ptr() as isize,
        ) as usize;
        String::from_utf16_lossy(&buf[..got])
    }
}

unsafe fn paint_control(hwnd: HWND, role: Role) {
    let mut ps: windows_sys::Win32::Graphics::Gdi::PAINTSTRUCT = std::mem::zeroed();
    let dc = BeginPaint(hwnd, &mut ps);
    let mut rc: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut rc);
    let w = rc.right - rc.left;
    let h = rc.bottom - rc.top;

    let st = with_state(|m| *m.get(&(hwnd as isize)).unwrap_or(&State::default()));
    let enabled = IsWindowEnabled(hwnd) != 0;

    // 背景：先铺主背景色（控件矩形是矩形，父窗口也是同色，所以看不出边界）
    FillRect(dc, &rc, bg_brush());

    let old_font = SelectObject(dc, font());
    SetBkMode(dc, 1); // TRANSPARENT

    match role {
        Role::Progress => {
            // 轨道 + 进度（进度值由 PBM_SETPOS 设置，这里用 PBM_GETPOS 读）
            let track = RECT {
                left: 0,
                top: (h - 6) / 2,
                right: w,
                bottom: (h + 6) / 2,
            };
            fill_round(dc, track, BORDER, 6);
            let pos = windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW(
                hwnd, 0x0408, // PBM_GETPOS
                0, 0,
            ) as i32;
            let pct = (pos as f64 / 100.0).clamp(0.0, 1.0);
            if pct > 0.0 {
                let fill = RECT {
                    left: 0,
                    top: track.top,
                    right: ((w as f64) * pct) as i32,
                    bottom: track.bottom,
                };
                fill_round(dc, fill, ACCENT, 6);
            }
        }
        Role::Check => {
            // 14x14 圆角勾选框 + 文字
            let cb = RECT {
                left: 2,
                top: (h - 15) / 2,
                right: 17,
                bottom: (h - 15) / 2 + 15,
            };
            let checked = windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW(
                hwnd, 0x00F0, // BM_GETCHECK
                0, 0,
            ) == 1;
            let bgc = if checked {
                ACCENT
            } else if st.hover {
                HOVER
            } else {
                PANEL
            };
            fill_round(dc, cb, bgc, 4);
            if checked {
                // 画一个 2px 的勾
                let pen = CreatePen(PS_SOLID, 2, 0x00FF_FFFF);
                let op = SelectObject(dc, pen);
                let pts = [
                    windows_sys::Win32::Foundation::POINT {
                        x: cb.left + 3,
                        y: cb.top + 7,
                    },
                    windows_sys::Win32::Foundation::POINT {
                        x: cb.left + 6,
                        y: cb.top + 10,
                    },
                    windows_sys::Win32::Foundation::POINT {
                        x: cb.left + 11,
                        y: cb.top + 4,
                    },
                ];
                windows_sys::Win32::Graphics::Gdi::Polyline(dc, pts.as_ptr(), pts.len() as i32);
                SelectObject(dc, op);
                DeleteObject(pen);
            }
            let text_rc = RECT {
                left: 24,
                top: 0,
                right: w,
                bottom: h,
            };
            control_label(
                dc,
                hwnd,
                text_rc,
                if enabled { TEXT } else { DISABLED },
                0x0000_0004, // DT_LEFT | DT_VCENTER
                w - 28,
            );
        }
        Role::Accent | Role::Ghost | Role::Danger => {
            let radius = 10;
            match role {
                Role::Accent => {
                    let c = if !enabled {
                        0x0040_6A42 // 暗绿
                    } else if st.pressed {
                        ACCENT_DOWN
                    } else if st.hover {
                        ACCENT_HOVER
                    } else {
                        ACCENT
                    };
                    fill_round(dc, rc, c, radius);
                }
                _ => {
                    let fill = if st.pressed {
                        DOWN
                    } else if st.hover {
                        HOVER
                    } else {
                        PANEL
                    };
                    fill_round(dc, rc, fill, radius);
                    // 1px 描边，让"幽灵按钮"在深色底上仍像一个按钮
                    let border = CreatePen(PS_SOLID, 1, BORDER);
                    let ob = SelectObject(dc, GetStockObject(NULL_BRUSH));
                    let op = SelectObject(dc, border);
                    RoundRect(
                        dc,
                        rc.left,
                        rc.top,
                        rc.right - 1,
                        rc.bottom - 1,
                        radius,
                        radius,
                    );
                    SelectObject(dc, op);
                    SelectObject(dc, ob);
                    DeleteObject(border);
                }
            }
            let fg = match role {
                Role::Accent => 0x00FF_FFFF,
                Role::Danger if st.hover => DANGER,
                _ => {
                    if enabled {
                        TEXT
                    } else {
                        DISABLED
                    }
                }
            };
            control_label(
                dc,
                hwnd,
                rc,
                fg,
                0x0000_0001 | 0x0000_0004, // DT_CENTER | DT_VCENTER
                w - 10,
            );
        }
    }

    SelectObject(dc, old_font);
    EndPaint(hwnd, &ps);
}

unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    role_raw: usize,
) -> LRESULT {
    let role = match role_raw {
        1 => Role::Accent,
        2 => Role::Danger,
        3 => Role::Check,
        4 => Role::Progress,
        _ => Role::Ghost,
    };
    win::guard(|| match msg {
        WM_PAINT => {
            paint_control(hwnd, role);
            0
        }
        WM_ERASEBKGND => 1,
        WM_MOUSEMOVE => {
            let changed = with_state(|m| {
                let e = m.entry(hwnd as isize).or_default();
                let c = !e.hover;
                e.hover = true;
                c
            });
            if changed {
                InvalidateRect(hwnd, std::ptr::null(), 0);
            }
            let mut tme: TRACKMOUSEEVENT = std::mem::zeroed();
            tme.cbSize = std::mem::size_of::<TRACKMOUSEEVENT>() as u32;
            tme.dwFlags = TME_LEAVE;
            tme.hwndTrack = hwnd;
            TrackMouseEvent(&mut tme);
            DefSubclassProc(hwnd, msg, wparam, lparam)
        }
        WM_MOUSELEAVE_CTRL => {
            with_state(|m| {
                if let Some(e) = m.get_mut(&(hwnd as isize)) {
                    e.hover = false;
                }
            });
            InvalidateRect(hwnd, std::ptr::null(), 0);
            DefSubclassProc(hwnd, msg, wparam, lparam)
        }
        WM_LBUTTONDOWN => {
            with_state(|m| {
                if let Some(e) = m.get_mut(&(hwnd as isize)) {
                    e.pressed = true;
                }
            });
            InvalidateRect(hwnd, std::ptr::null(), 0);
            DefSubclassProc(hwnd, msg, wparam, lparam)
        }
        WM_LBUTTONUP => {
            with_state(|m| {
                if let Some(e) = m.get_mut(&(hwnd as isize)) {
                    e.pressed = false;
                }
            });
            InvalidateRect(hwnd, std::ptr::null(), 0);
            DefSubclassProc(hwnd, msg, wparam, lparam)
        }
        WM_ENABLE | WM_SETFOCUS | WM_KILLFOCUS => {
            InvalidateRect(hwnd, std::ptr::null(), 0);
            DefSubclassProc(hwnd, msg, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, msg, wparam, lparam),
    })
}

/// 把一个子控件挂上自绘主题（按钮 / 复选框 / 进度条）
pub unsafe fn decorate(hwnd: HWND, role: Role) {
    if hwnd.is_null() {
        return;
    }
    let raw = match role {
        Role::Ghost => 0,
        Role::Accent => 1,
        Role::Danger => 2,
        Role::Check => 3,
        Role::Progress => 4,
    };
    SetWindowSubclass(hwnd, Some(subclass_proc), 1, raw);
    InvalidateRect(hwnd, std::ptr::null(), 0);
}

// ---------------------------------------------------------------------------
// 让系统控件（下拉框 / 输入框）跟随深色
// ---------------------------------------------------------------------------

fn uxtheme() -> isize {
    static H: OnceLock<isize> = OnceLock::new();
    *H.get_or_init(|| unsafe {
        let name = win::wide("uxtheme.dll");
        // 不能使用裸 LoadLibraryW：便携版 exe 常放在可写目录，Windows 会先搜索
        // exe 所在目录，存在 DLL 搜索路径劫持风险。只允许系统目录。
        windows_sys::Win32::System::LibraryLoader::LoadLibraryExW(
            name.as_ptr(),
            std::ptr::null_mut(),
            0x0000_0800, // LOAD_LIBRARY_SEARCH_SYSTEM32
        ) as isize
    })
}

/// uxtheme 的未公开导出（Win10 1809+ / Win11 都有；失败返回 0）
unsafe fn ordinal(n: usize) -> usize {
    let h = uxtheme();
    if h == 0 {
        return 0;
    }
    match windows_sys::Win32::System::LibraryLoader::GetProcAddress(h as _, n as *const u8) {
        Some(f) => f as usize,
        None => 0,
    }
}

/// 进程级：偏好深色（2 = ForceDark）
pub unsafe fn prefer_dark() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let f = ordinal(135);
        if f != 0 {
            let f: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            f(2);
        }
    });
}

/// 单个窗口：允许深色 + 换成 DarkMode_Explorer 主题
#[allow(dead_code)]
pub unsafe fn darken(hwnd: HWND) {
    if hwnd.is_null() {
        return;
    }
    prefer_dark();
    let allow = ordinal(133);
    if allow != 0 {
        let allow: unsafe extern "system" fn(HWND, i32) -> i32 = std::mem::transmute(allow);
        allow(hwnd, 1);
    }
    let cls = win::wide("DarkMode_Explorer");
    SetWindowTheme(hwnd, cls.as_ptr(), std::ptr::null());
    // 输入框/下拉框重绘一次，让新主题生效
    InvalidateRect(hwnd, std::ptr::null(), 1);
}

/// 静态文字控件：深色底 + 浅色字（在主窗口 `WM_CTLCOLORSTATIC` 里调用）
pub unsafe fn paint_static(dc: HDC) -> LRESULT {
    SetBkMode(dc, 1); // TRANSPARENT
    SetTextColor(dc, TEXT);
    bg_brush() as LRESULT
}

/// 主窗口：深色标题栏 + 无系统边框色 + 圆角
pub unsafe fn dark_window_frame(hwnd: HWND) {
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
    let on: u32 = 1;
    let _ = DwmSetWindowAttribute(hwnd, 20, &on as *const u32 as *const _, 4); // DARK_MODE
    let pref: u32 = 2;
    let _ = DwmSetWindowAttribute(hwnd, 33, &pref as *const u32 as *const _, 4); // 圆角
    let none: u32 = 0xFFFF_FFFE;
    let _ = DwmSetWindowAttribute(hwnd, 34, &none as *const u32 as *const _, 4);
    // 无边框色
}

/// 输入框（Edit）：深色底 + 浅色字。在主窗口 `WM_CTLCOLOREDIT` 里调用。
pub unsafe fn paint_edit(dc: HDC) -> LRESULT {
    SetBkMode(dc, 2); // OPAQUE
    SetBkColor(dc, BG);
    SetTextColor(dc, TEXT);
    bg_brush() as LRESULT
}
