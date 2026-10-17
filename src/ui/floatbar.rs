//! 悬浮操作条：无边框 / 置顶 / **不抢焦点** / 整条可拖动。
//!
//! 三条关键的原生手段：
//! * `WS_EX_NOACTIVATE` —— 点它**不会**把焦点从绘画软件抢走（这是产品硬需求：
//!   艺术家在画布上落笔时焦点不能被夺走）；
//! * `WS_EX_TOOLWINDOW` —— 不出现在 Alt-Tab 与任务栏里；
//! * `WM_NCHITTEST` 一律返回 `HTCAPTION` —— 于是按住条上任意位置都能拖动。
//!
//! 按钮文字用纯中文而不是 ⏸🏷⏹：系统默认 GUI 字体不保证包含这些符号字形，
//! 用了会变成豆腐块（这一点在 egui 路线上已实测踩到）。

use std::sync::{Arc, Mutex};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{GetStockObject, DEFAULT_GUI_FONT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetDlgItem, RegisterClassW, SetTimer,
    SetWindowPos, SetWindowTextW, ShowWindow, GWLP_USERDATA, HTCAPTION, HWND_TOPMOST, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SW_SHOWNOACTIVATE, WM_COMMAND, WM_DESTROY, WM_NCHITTEST, WM_SETFONT,
    WM_TIMER, WNDCLASSW, WS_CHILD, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    WS_VISIBLE,
};

use crate::recorder::{self, Shared};
use crate::win;

const BAR_CLASS: &str = "CanLapseFloatBar";
const ID_PAUSE: usize = 2001;
const ID_MARK: usize = 2002;
const ID_STOP: usize = 2003;
/// 时长 / 帧计数 的显示控件。
/// 注意：**不能**只写进窗口标题 —— 悬浮条是 WS_POPUP 无边框窗口，没有标题栏，
/// 用户根本看不到（这个坑是靠截图发现的）。
const ID_INFO: usize = 2004;
const TIMER_REFRESH: usize = 1;

const BAR_W: i32 = 400;
const BAR_H: i32 = 44;
const BTN_W: i32 = 58;
const BTN_H: i32 = 28;

/// 悬浮条是进程内单例，共享状态放在静态里，避免往 GWLP_USERDATA 塞 Arc 裸指针。
static BAR_SHARED: Mutex<Option<Arc<Shared>>> = Mutex::new(None);

fn btn_class() -> Vec<u16> {
    win::wide("Button")
}

fn static_class() -> Vec<u16> {
    win::wide("Static")
}

unsafe fn add_child(
    parent: HWND,
    class: &[u16],
    text: &str,
    id: usize,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> HWND {
    let t = win::wide(text);
    let hwnd = CreateWindowExW(
        0,
        class.as_ptr(),
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
    // 用系统默认 GUI 字体，避免中文变豆腐块
    let font = GetStockObject(DEFAULT_GUI_FONT);
    windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW(
        hwnd,
        WM_SETFONT,
        font as WPARAM,
        1,
    );
    hwnd
}

unsafe extern "system" fn bar_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // 整条都能拖动
        WM_NCHITTEST => HTCAPTION as LRESULT,
        WM_COMMAND => {
            let id = (wparam & 0xffff) as usize;
            if let Ok(guard) = BAR_SHARED.lock() {
                if let Some(shared) = guard.as_ref() {
                    match id {
                        ID_PAUSE => {
                            shared.toggle_pause();
                        }
                        ID_MARK => shared.request_mark(),
                        ID_STOP => shared.request_stop(),
                        _ => {}
                    }
                }
            }
            0
        }
        WM_TIMER => {
            if wparam == TIMER_REFRESH {
                refresh(hwnd);
            }
            0
        }
        WM_DESTROY => 0,
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 刷新条上的暂停按钮文字与"时长 | 帧计数"
unsafe fn refresh(hwnd: HWND) {
    let Ok(guard) = BAR_SHARED.lock() else { return };
    let Some(shared) = guard.as_ref() else { return };
    let paused = shared.is_paused();
    let pause_btn = GetDlgItem(hwnd, ID_PAUSE as i32);
    let t = win::wide(if paused { "继续" } else { "暂停" });
    SetWindowTextW(pause_btn, t.as_ptr());
    // 时长 | 帧计数 | 标记数 显示在条标题上（条很窄，标题栏本来就空着）
    // 状态提示必须出现在**这里**：录制时主窗口已最小化，悬浮条是用户唯一能看到的界面。
    let info = format!(
        "{} | {} 帧{}{}",
        recorder::format_duration(shared.elapsed_ms()),
        shared.frames(),
        if shared.markers() > 0 {
            format!(" | {} 标记", shared.markers())
        } else {
            String::new()
        },
        // 用户规则：别的软件在前台 => 此刻没在画画 => 自动暂停抓帧（切回即继续）
        if shared.fg_paused() {
            " | ⏸ 其他软件在前台"
        } else {
            ""
        }
    );
    // 写进可见的 Static 控件（用户看的是这里），标题也一并更新（便于自动化识别）
    let info_ctl = GetDlgItem(hwnd, ID_INFO as i32);
    let t = win::wide(&info);
    SetWindowTextW(info_ctl, t.as_ptr());
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
        hbrBackground: windows_sys::Win32::Graphics::Gdi::CreateSolidBrush(0x0020_2020)
            as _,
        lpszMenuName: std::ptr::null(),
        lpszClassName: cls.as_ptr(),
    };
    RegisterClassW(&wc);
}

/// 创建悬浮条（默认放在屏幕左下角偏上位置）
pub fn create(shared: Arc<Shared>) -> Option<HWND> {
    if let Ok(mut g) = BAR_SHARED.lock() {
        *g = Some(shared);
    }
    unsafe {
        ensure_class();
        let cls = win::wide(BAR_CLASS);
        let title = win::wide("CanLapse");
        let x = 40;
        let y = 120;
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            cls.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            x,
            y,
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
        let pad = 6;
        let bc = btn_class();
        add_child(hwnd, &bc, "暂停", ID_PAUSE, pad, pad, BTN_W, BTN_H);
        add_child(hwnd, &bc, "标记", ID_MARK, pad + BTN_W + 4, pad, BTN_W, BTN_H);
        add_child(hwnd, &bc, "停止", ID_STOP, pad + (BTN_W + 4) * 2, pad, BTN_W, BTN_H);
        // 时长 | 帧计数：必须是**可见的控件**，不能只放窗口标题
        let info_x = pad + (BTN_W + 4) * 3;
        let sc = static_class();
        add_child(hwnd, &sc, "00:00 | 0 帧", ID_INFO, info_x, pad + 5, BAR_W - info_x - pad, 18);
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
        refresh(hwnd);
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
