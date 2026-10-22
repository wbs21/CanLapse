//! Win32 基础封装：窗口枚举、进程名、鼠标拾取、消息泵、本地时间。
//!
//! 这里的公共函数几乎都接收 `HWND`（裸指针句柄）。调用者本身就在 unsafe 的
//! Win32 上下文中，逐个函数标 unsafe 只会把噪声扩散到 UI 层，因此在模块级别
//! 接受 clippy 的该项检查；真正的内存安全边界仍是各 `unsafe` 块。
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Once;
use std::time::Duration;

use crate::logline;

use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, HWND, LPARAM, LRESULT, POINT, RECT, SYSTEMTIME, WPARAM,
};
// 注意：ClientToScreen 在 Graphics::Gdi 下，不在 WindowsAndMessaging（实测踩过）。
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint,
    FillRect, FrameRect, GetMonitorInfoW, MonitorFromWindow, SelectObject, SetBkMode, SetTextColor,
    DEFAULT_CHARSET, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::SystemInformation::{GetLocalTime, GetWindowsDirectoryW};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE, VK_F8};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EnumChildWindows,
    EnumWindows, GetAncestor, GetClassNameW, GetClientRect, GetCursorPos, GetForegroundWindow,
    GetParent, GetSystemMetrics, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindow, IsWindowVisible, PeekMessageW, RegisterClassW, SetLayeredWindowAttributes,
    SetWindowPos, ShowWindow, SystemParametersInfoW, TranslateMessage, WindowFromPoint, GA_ROOT,
    MSG, PM_REMOVE, SW_HIDE, SW_SHOWNOACTIVATE, WM_LBUTTONDOWN, WM_PAINT, WM_RBUTTONDOWN,
    WNDCLASSW, WS_POPUP,
};

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// FFI 窗口回调的兜底壳：panic 一旦跨过 `extern "system"` 边界会直接 abort 进程，
/// 用户看到的就是"程序突然没了"。这里接住、记日志、返回 0，让进程活下去。
pub fn guard<F: FnOnce() -> LRESULT>(f: F) -> LRESULT {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(e) => {
            let msg = if let Some(s) = e.downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = e.downcast_ref::<String>() {
                s.clone()
            } else {
                "未知 panic".to_string()
            };
            logline!("[panic] 窗口回调: {msg}");
            0
        }
    }
}

#[derive(Clone)]
pub struct WindowInfo {
    pub hwnd: HWND,
    pub title: String,
    pub class_name: String,
    pub pid: u32,
    pub process: String,
    pub rect: RECT,
    pub client: RECT,
    pub minimized: bool,
}

impl WindowInfo {
    /// 给人看的单行描述（拾取窗口与历史列表都会用到）
    pub fn label(&self) -> String {
        let t = if self.title.is_empty() {
            self.class_name.clone()
        } else {
            self.title.clone()
        };
        let is_child = unsafe { !GetParent(self.hwnd).is_null() };
        format!(
            "{}{} [{}] {}x{}",
            t,
            if is_child { "（子窗口）" } else { "" },
            if self.process.is_empty() {
                "?"
            } else {
                &self.process
            },
            self.client.right - self.client.left,
            self.client.bottom - self.client.top
        )
    }

    pub fn client_size(&self) -> (i32, i32) {
        (
            self.client.right - self.client.left,
            self.client.bottom - self.client.top,
        )
    }
}

pub fn process_name(pid: u32) -> String {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return String::new();
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(h);
        if ok == 0 {
            return String::new();
        }
        let s = String::from_utf16_lossy(&buf[..len as usize]);
        s.rsplit(['\\', '/']).next().unwrap_or("").to_lowercase()
    }
}

pub fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

pub fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

pub fn describe(hwnd: HWND) -> WindowInfo {
    unsafe {
        let mut rect: RECT = std::mem::zeroed();
        GetWindowRect(hwnd, &mut rect);
        let mut client: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &mut client);
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        WindowInfo {
            hwnd,
            title: window_title(hwnd),
            class_name: class_name(hwnd),
            pid,
            process: process_name(pid),
            rect,
            client,
            minimized: IsIconic(hwnd) != 0,
        }
    }
}

/// 注意：HWND 是裸指针，不能放进 `static Mutex<..>`（不满足 Sync）。
/// 所以结果通过 EnumWindows 的 lparam 用栈上的 Vec 收集，而不是全局静态。
unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if IsWindowVisible(hwnd) == 0 {
            return;
        }
        let out = &mut *(lparam as *mut Vec<WindowInfo>);
        let info = describe(hwnd);
        if info.title.is_empty() && info.class_name.is_empty() {
            return;
        }
        out.push(info);
    }));
    1
}

/// 枚举可见的顶层窗口（按客户区面积从大到小）。
/// 注意：DWM 缩略图**不要求**窗口可见，但"拾取"这一步给用户看时必须挑可见窗口。
pub fn list_windows() -> Vec<WindowInfo> {
    let mut out: Vec<WindowInfo> = Vec::new();
    unsafe {
        EnumWindows(Some(enum_cb), &mut out as *mut Vec<WindowInfo> as LPARAM);
    }
    out.sort_by_key(|w| {
        let (cw, ch) = w.client_size();
        -(cw as i64 * ch as i64)
    });
    out
}

unsafe extern "system" fn child_enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if IsWindowVisible(hwnd) == 0 {
            return;
        }
        let out = &mut *(lparam as *mut Vec<HWND>);
        out.push(hwnd);
    }));
    1
}

/// 枚举某个顶层窗口的全部可见后代，按客户区面积从大到小。
pub fn list_child_windows(root: HWND) -> Vec<WindowInfo> {
    let mut handles: Vec<HWND> = Vec::new();
    unsafe {
        EnumChildWindows(
            root,
            Some(child_enum_cb),
            &mut handles as *mut Vec<HWND> as LPARAM,
        );
    }
    let mut out: Vec<WindowInfo> = handles
        .into_iter()
        .map(describe)
        .filter(|w| !w.title.is_empty() || !w.class_name.is_empty())
        .collect();
    out.sort_by_key(|w| {
        let (cw, ch) = w.client_size();
        -(cw as i64 * ch as i64)
    });
    out
}

/// 按标题子串 / 类名 / 进程名查找（用于命令行指定目标）
pub fn find_window(spec: &str) -> Option<WindowInfo> {
    let low = spec.to_lowercase();
    list_windows().into_iter().find(|w| {
        w.title.to_lowercase().contains(&low)
            || w.class_name.to_lowercase().contains(&low)
            || w.process.to_lowercase().contains(&low)
    })
}

pub fn is_window_alive(hwnd: HWND) -> bool {
    !hwnd.is_null() && unsafe { IsWindow(hwnd) != 0 }
}

pub fn is_minimized(hwnd: HWND) -> bool {
    unsafe { IsIconic(hwnd) != 0 }
}

pub fn client_size(hwnd: HWND) -> (i32, i32) {
    let mut rc: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetClientRect(hwnd, &mut rc) };
    (rc.right - rc.left, rc.bottom - rc.top)
}

/// 客户端区域在屏幕上的矩形（预览/高亮用）
pub fn client_screen_rect(hwnd: HWND) -> RECT {
    let mut rc: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetClientRect(hwnd, &mut rc) };
    let mut pt = POINT { x: 0, y: 0 };
    unsafe { ClientToScreen(hwnd, &mut pt) };
    RECT {
        left: pt.x,
        top: pt.y,
        right: pt.x + (rc.right - rc.left),
        bottom: pt.y + (rc.bottom - rc.top),
    }
}

fn visible_ratio(rc: &RECT, vx: i32, vy: i32, vw: i32, vh: i32) -> f32 {
    let width = (rc.right - rc.left).max(0);
    let height = (rc.bottom - rc.top).max(0);
    if vw <= 0 || vh <= 0 || width == 0 || height == 0 {
        return 0.0;
    }
    let visible_w = rc.right.min(vx + vw).saturating_sub(rc.left.max(vx)).max(0);
    let visible_h = rc.bottom.min(vy + vh).saturating_sub(rc.top.max(vy)).max(0);
    (visible_w as i64 * visible_h as i64) as f32 / (width as i64 * height as i64) as f32
}

/// 客户端区域在虚拟桌面内的可见比例（0.0~1.0）。
pub fn client_visible_ratio(hwnd: HWND) -> f32 {
    let rc = client_screen_rect(hwnd);
    let vx = unsafe { GetSystemMetrics(76) }; // SM_XVIRTUALSCREEN
    let vy = unsafe { GetSystemMetrics(77) }; // SM_YVIRTUALSCREEN
    let vw = unsafe { GetSystemMetrics(78) }; // SM_CXVIRTUALSCREEN
    let vh = unsafe { GetSystemMetrics(79) }; // SM_CYVIRTUALSCREEN
    visible_ratio(&rc, vx, vy, vw, vh)
}

/// DWM 在源窗口只剩很窄边缘时可能停止刷新；这类窗口改用直接 PrintWindow。
pub fn capture_needs_direct(hwnd: HWND) -> bool {
    let rc = client_screen_rect(hwnd);
    let width = (rc.right - rc.left).max(0);
    let height = (rc.bottom - rc.top).max(0);
    client_visible_ratio(hwnd) < 0.999 || width < 64 || height < 64
}

// ---------------------------------------------------------------------------
// 消息泵
// ---------------------------------------------------------------------------

/// 处理掉所有待处理消息（不阻塞）。宿主窗口需要它保持"活着"。
pub fn pump() {
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// 边抽消息边等待
pub fn settle(ms: u64) {
    let step = 10u64;
    let mut left = ms;
    while left > 0 {
        pump();
        std::thread::sleep(Duration::from_millis(step.min(left)));
        left = left.saturating_sub(step);
    }
}

// ---------------------------------------------------------------------------
// 鼠标拾取窗口
// ---------------------------------------------------------------------------

fn key_state(vk: u16) -> u16 {
    unsafe { GetAsyncKeyState(vk as i32) as u16 }
}

fn key_down(vk: u16) -> bool {
    key_state(vk) & 0x8000 != 0
}

/// 拾取时跟随鼠标的提示条 —— **自绘的置顶小窗**。
///
/// 为什么不用 Win32 tooltip：实测在本场景它**根本显示不出来**
/// （owner 已最小化 + TTF_TRACK + 进程无焦点；截图确认光标旁什么都没有）。
/// 用户看不到"要点画布"，就转而去点任务栏把主窗口叫回来，拾取随即被取消 ——
/// 日志里两次拾取都是这么结束的。自绘窗口多写几十行，但置顶/半透明/鼠标穿透/不抢焦点
/// 全部可控。
const HINT_CLASS: &str = "CanLapsePickHint";
static HINT_REG: Once = Once::new();
const HINT_W: i32 = 240;
const HINT_H: i32 = 40;

unsafe extern "system" fn hint_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    guard(|| match msg {
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut rc);
            let bg = CreateSolidBrush(0x0026_1F1F);
            FillRect(hdc, &rc, bg);
            DeleteObject(bg as HGDIOBJ);
            let border = CreateSolidBrush(0x0060_6060);
            FrameRect(hdc, &rc, border);
            DeleteObject(border as HGDIOBJ);
            let mut buf = [0u16; 256];
            let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
            if n > 0 {
                let face = wide("Microsoft YaHei UI");
                let font = CreateFontW(
                    -15,
                    0,
                    0,
                    0,
                    500,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET as u32,
                    0,
                    0,
                    0,
                    0,
                    face.as_ptr(),
                );
                let old = SelectObject(hdc, font as HGDIOBJ);
                SetBkMode(hdc, 1); // TRANSPARENT
                SetTextColor(hdc, 0x00F0_F0F0);
                let mut trc = RECT {
                    left: 10,
                    top: 6,
                    right: rc.right - 10,
                    bottom: rc.bottom - 4,
                };
                // DT_CENTER | DT_VCENTER | DT_SINGLELINE
                DrawTextW(
                    hdc,
                    buf.as_ptr(),
                    n,
                    &mut trc,
                    0x0000_0001 | 0x0000_0004 | 0x0000_0020,
                );
                SelectObject(hdc, old);
                DeleteObject(font as HGDIOBJ);
            }
            EndPaint(hwnd, &ps);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    })
}

/// 提示条相对光标的落点，贴到屏幕边缘时翻到另一侧
fn hint_pos(cx: i32, cy: i32) -> (i32, i32) {
    let (vx, vy, vw, vh) = virtual_screen();
    let mut x = cx + 18;
    let mut y = cy + 24;
    if x + HINT_W > vx + vw {
        x = cx - HINT_W - 12;
    }
    if y + HINT_H > vy + vh {
        y = cy - HINT_H - 12;
    }
    (x.max(vx), y.max(vy))
}

fn virtual_screen() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(76), // SM_XVIRTUALSCREEN
            GetSystemMetrics(77), // SM_YVIRTUALSCREEN
            GetSystemMetrics(78), // SM_CXVIRTUALSCREEN
            GetSystemMetrics(79), // SM_CYVIRTUALSCREEN
        )
    }
}

pub fn pick_hint_create(text: &str) -> HWND {
    HINT_REG.call_once(|| unsafe {
        let c = wide(HINT_CLASS);
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(hint_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: module_handle(),
            hIcon: null_mut(),
            hCursor: null_mut(),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: c.as_ptr(),
        };
        RegisterClassW(&wc);
    });
    let c = wide(HINT_CLASS);
    let t = wide(text);
    unsafe {
        // LAYERED | TOPMOST | TOOLWINDOW | TRANSPARENT | NOACTIVATE
        let tip = CreateWindowExW(
            0x0008_0000 | 0x0000_0008 | 0x0000_0080 | 0x0000_0020 | 0x0800_0000,
            c.as_ptr(),
            t.as_ptr(),
            WS_POPUP,
            0,
            0,
            HINT_W,
            HINT_H,
            // ⚠️ 不能挂 owner：owner 最小化时 Windows 会把 owned 窗口一起隐藏
            null_mut(),
            null_mut(),
            module_handle(),
            null(),
        );
        if tip.is_null() {
            logline!("[pick] 提示条创建失败");
            return tip;
        }
        SetLayeredWindowAttributes(tip, 0, 240, 0x2);
        ShowWindow(tip, SW_SHOWNOACTIVATE);
        let p = cursor_position();
        let (x, y) = hint_pos(p.0, p.1);
        SetWindowPos(tip, (-1isize) as HWND, x, y, 0, 0, 0x0001 | 0x0010);
        tip
    }
}

/// 让提示条跟随鼠标（拾取循环里每次迭代调用）
pub fn pick_hint_follow(tip: HWND) {
    if tip.is_null() {
        return;
    }
    let p = cursor_position();
    let (x, y) = hint_pos(p.0, p.1);
    unsafe {
        SetWindowPos(tip, (-1isize) as HWND, x, y, 0, 0, 0x0001 | 0x0010);
    }
}

pub fn pick_hint_destroy(tip: HWND) {
    if tip.is_null() {
        return;
    }
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow(tip);
    }
}

/// 拾取提示条的 RAII 包装：即使拾取流程里发生 panic，也不会把 topmost
/// 提示条永久留在桌面上。
pub struct PickHintGuard(pub HWND);

impl Drop for PickHintGuard {
    fn drop(&mut self) {
        pick_hint_destroy(self.0);
    }
}

fn cursor_position() -> (i32, i32) {
    unsafe {
        let mut p: POINT = std::mem::zeroed();
        GetCursorPos(&mut p);
        (p.x, p.y)
    }
}

static MARK_KEY_DOWN: AtomicBool = AtomicBool::new(false);
static ESC_KEY_DOWN: AtomicBool = AtomicBool::new(false);

/// 录制循环里用：F8 是否**刚被按下**（边沿触发，按住不会连打标记）。
///
/// 规格里的 🏷 标记按钮在悬浮操作条上；命令行切片阶段用 F8 作为等价入口。
pub fn marker_key_pressed() -> bool {
    let state = key_state(VK_F8);
    let down = state & 0x8000 != 0;
    // 低 bit 表示“自上次查询以来按下过”，可捕获两次 UI tick 之间的短按。
    let pressed = state & 0x0001 != 0;
    let was = MARK_KEY_DOWN.swap(down, Ordering::SeqCst);
    pressed || (down && !was)
}

/// 与 [`marker_key_pressed`] 相同的边沿触发语义：只在**刚按下** Esc 时返回 true。
///
/// GUI 的主窗口在录制时是最小化的，键盘消息不会送到这里，所以只能在 UI 定时器里
/// 轮询 `GetAsyncKeyState`。用边沿触发可以避免按住 Esc 时每个 200ms tick 都请求停止。
pub fn escape_pressed_once() -> bool {
    let state = key_state(VK_ESCAPE);
    let down = state & 0x8000 != 0;
    let pressed = state & 0x0001 != 0;
    let was = ESC_KEY_DOWN.swap(down, Ordering::SeqCst);
    pressed || (down && !was)
}

// ---------------------------------------------------------------------------
// 拾取点击捕获：全屏透明覆盖层
// ---------------------------------------------------------------------------
//
// 为什么弃用 WH_MOUSE_LL：实测在本程序里钩子**安装成功**、但一个点击都收不到
// （用户点画布边框无数次，日志里零条 [hook]；GetAsyncKeyState 也读不到按下）。
// 低级钩子是否投递取决于安装线程的消息泵时序，不可依赖。
//
// 覆盖层是确定性的：拾取时铺一张覆盖整个虚拟桌面的窗口，点击必然落在它身上；
// 我们记录坐标、立刻隐藏自己，再用 WindowFromPoint 取该点下的真实窗口 ——
// 目标软件一次点击都收不到（不会在画布上画点），也不依赖钩子或按键轮询。
const OVERLAY_CLASS: &str = "CanLapsePickOverlay";
static OVERLAY_REG: Once = Once::new();
static OVERLAY_HIT: AtomicBool = AtomicBool::new(false);
static OVERLAY_CANCEL: AtomicBool = AtomicBool::new(false);
static OVERLAY_X: AtomicIsize = AtomicIsize::new(0);
static OVERLAY_Y: AtomicIsize = AtomicIsize::new(0);
/// 最后一次成功拾取的点击点（do_pick 用它判断"点到边框还是画面"）
unsafe extern "system" fn overlay_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    guard(|| match msg {
        WM_LBUTTONDOWN => {
            let (vx, vy, _, _) = virtual_screen();
            let x = vx + ((lp as i32) & 0xffff);
            let y = vy + ((lp >> 16) as i32);
            OVERLAY_X.store(x as isize, Ordering::SeqCst);
            OVERLAY_Y.store(y as isize, Ordering::SeqCst);
            OVERLAY_HIT.store(true, Ordering::SeqCst);
            // 先隐藏自己，下面才可能 WindowFromPoint 到真正的窗口
            ShowWindow(hwnd, SW_HIDE);
            logline!("[pick] 覆盖层收到左键 ({x},{y})");
            0
        }
        WM_RBUTTONDOWN => {
            OVERLAY_CANCEL.store(true, Ordering::SeqCst);
            ShowWindow(hwnd, SW_HIDE);
            logline!("[pick] 覆盖层收到右键：取消拾取");
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    })
}

/// 铺一张覆盖整个虚拟桌面的透明窗口（几乎不可见，但**参与命中测试**）
unsafe fn overlay_create() -> HWND {
    OVERLAY_REG.call_once(|| {
        let c = wide(OVERLAY_CLASS);
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(overlay_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: module_handle(),
            hIcon: null_mut(),
            hCursor: null_mut(),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: c.as_ptr(),
        };
        RegisterClassW(&wc);
    });
    let (vx, vy, vw, vh) = virtual_screen();
    let c = wide(OVERLAY_CLASS);
    // TOPMOST | TOOLWINDOW | LAYERED | NOACTIVATE
    // ⚠️ 不能加 WS_EX_TRANSPARENT，那会让点击穿透到目标软件
    let h = CreateWindowExW(
        0x0000_0008 | 0x0000_0080 | 0x0008_0000 | 0x0800_0000,
        c.as_ptr(),
        null(),
        WS_POPUP,
        vx,
        vy,
        vw,
        vh,
        null_mut(),
        null_mut(),
        module_handle(),
        null(),
    );
    if h.is_null() {
        logline!("[pick] 覆盖层创建失败，无法捕获点击");
        return h;
    }
    // 20/255 的均匀透明度：几乎看不见，但整块窗口仍然可命中
    SetLayeredWindowAttributes(h, 0, 20, 0x2);
    ShowWindow(h, SW_SHOWNOACTIVATE);
    SetWindowPos(h, (-1isize) as HWND, vx, vy, vw, vh, 0x0010);
    logline!("[pick] 覆盖层已铺开 {vw}x{vh} @({vx},{vy})");
    h
}

struct OverlayGuard(HWND);

impl Drop for OverlayGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                DestroyWindow(self.0);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 点击目标归位（借鉴 add/chose.rs 的"候选画布"思路，但只用于归位）
// ---------------------------------------------------------------------------
//
// 参考实现先把"看起来像画布"的子窗口收集成候选，再用点击点去匹配。我们只借它做**归位**，
// 不做"自动识别画布"（需求明确不做）：
//   1. 命中的是画布内部更深的子窗 → 上溯到最近的候选（否则可能录到一条细缝/渲染子窗）；
//   2. 命中的是根窗口、且点击落在某候选的 6px 边框环上 → 归位到该候选
//      （用户习惯"点画布边框"来选中，这条正对这个手势）；
//   3. 其余保持"点哪选哪"。
//
// 候选条件故意从宽（可见 + ≥100×100），**不把 clip 样式当硬条件**：
// 参考实现里那条过滤挡不住 201×955 的工具条，拿它当判据会把工具条也当画布。

const CAND_MIN: i32 = 100;
pub const BORDER_PX: i32 = 6;

#[cfg(test)]
fn point_in_rect(pt: POINT, rc: &RECT) -> bool {
    pt.x >= rc.left && pt.x < rc.right && pt.y >= rc.top && pt.y < rc.bottom
}

/// 点是否落在矩形边缘 `px` 像素的"环"内（含外侧 `px`，不含内部超过 `px` 的位置）
fn point_near_edge(pt: POINT, rc: &RECT, px: i32) -> bool {
    let outer = pt.x >= rc.left - px
        && pt.x <= rc.right + px
        && pt.y >= rc.top - px
        && pt.y <= rc.bottom + px;
    if !outer {
        return false;
    }
    let inner = pt.x >= rc.left + px
        && pt.x <= rc.right - px
        && pt.y >= rc.top + px
        && pt.y <= rc.bottom - px;
    !inner
}

unsafe extern "system" fn cand_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let out = &mut *(lparam as *mut Vec<HWND>);
        if IsWindowVisible(hwnd) == 0 {
            return;
        }
        let mut rc: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut rc) == 0 {
            return;
        }
        if rc.right - rc.left < CAND_MIN || rc.bottom - rc.top < CAND_MIN {
            return;
        }
        out.push(hwnd);
    }));
    1
}

fn canvas_candidates(root: HWND) -> Vec<HWND> {
    let mut out: Vec<HWND> = Vec::new();
    unsafe { EnumChildWindows(root, Some(cand_cb), &mut out as *mut Vec<HWND> as LPARAM) };
    out
}

fn rect_of(h: HWND) -> Option<RECT> {
    let mut rc: RECT = unsafe { std::mem::zeroed() };
    if unsafe { GetWindowRect(h, &mut rc) } == 0 {
        None
    } else {
        Some(rc)
    }
}

/// 点击归位：返回最终目标 + 一句说明（写日志/状态栏用）
pub fn resolve_click(root: HWND, hit: HWND, pt: POINT) -> (HWND, &'static str) {
    if hit.is_null() {
        return (hit, "无窗口");
    }
    let cands = canvas_candidates(root);
    // 1) 直接点中候选
    if cands.contains(&hit) {
        return (hit, "直接命中（≥100×100 的子窗口）");
    }
    // 2) 内层子窗 → 最近的候选祖先
    let mut a = hit;
    for _ in 0..8 {
        a = unsafe { GetParent(a) };
        if a.is_null() || a == root {
            break;
        }
        if cands.contains(&a) {
            return (a, "命中内层子窗，上溯到候选窗口");
        }
    }
    // 3) 点到了根/空白：看是否点在候选的边框环上
    if hit == root {
        let mut ring: Vec<HWND> = Vec::new();
        for c in &cands {
            if let Some(rc) = rect_of(*c) {
                if point_near_edge(pt, &rc, BORDER_PX) {
                    ring.push(*c);
                }
            }
        }
        if ring.len() == 1 {
            return (ring[0], "点到边框环，归位到候选窗口");
        }
        if ring.len() > 1 {
            // 多个候选的边框都挨着点击点（例如画布和工具条只隔几像素）：**不猜**，
            // 保持点击命中的窗口，并在日志里说明 —— 猜错会锁到工具条上。
            return (hit, "多个候选边框相邻，保持命中窗口");
        }
    }
    (hit, "保持点击命中的窗口")
}

/// 交互式拾取：光标移到目标上，**点一下**确认；Esc / 右键取消。
///
/// `owner` 是发起拾取的窗口（已最小化）。它一旦回到前台就取消拾取 ——
/// Alt+Tab 等操作会把窗口叫回来，那时用户显然不是想继续拾取。
pub fn pick_window_interactive(owner: HWND, hint: Option<HWND>) -> Option<HWND> {
    let me = std::process::id();
    OVERLAY_HIT.store(false, Ordering::SeqCst);
    OVERLAY_CANCEL.store(false, Ordering::SeqCst);
    let overlay = unsafe { overlay_create() };
    if overlay.is_null() {
        return None;
    }
    let _overlay_guard = OverlayGuard(overlay);
    let mut ticks = 0u32;
    let mut result: Option<HWND> = None;
    logline!("[pick] 进入拾取（覆盖层捕获点击；Esc / 右键取消）");
    loop {
        pump();
        ticks += 1;
        // 提示条必须一直在覆盖层之上
        if let Some(tip) = hint {
            pick_hint_follow(tip);
        }
        if key_down(VK_ESCAPE) {
            logline!("[pick] Esc 取消");
            break;
        }
        // 最小化有极短的过渡期，前 10 轮（约 0.6s）不判"被恢复"
        if ticks > 10 && !owner.is_null() && unsafe { IsIconic(owner) } == 0 {
            logline!("[pick] 主窗口回到了前台，取消拾取（请重新点【选择画布窗口】）");
            break;
        }
        if OVERLAY_CANCEL.swap(false, Ordering::SeqCst) {
            break;
        }
        if OVERLAY_HIT.swap(false, Ordering::SeqCst) {
            let x = OVERLAY_X.load(Ordering::SeqCst) as i32;
            let y = OVERLAY_Y.load(Ordering::SeqCst) as i32;
            let pt = POINT { x, y };
            // 覆盖层已在点击处理里隐藏，这里取该点下真正的窗口
            let hit = unsafe { WindowFromPoint(pt) };
            if hit.is_null() {
                logline!("[pick] 点击点 ({x},{y}) 下没有窗口，继续拾取");
                unsafe {
                    ShowWindow(overlay, SW_SHOWNOACTIVATE);
                }
            } else if pid_of(hit) == me {
                logline!(
                    "[pick] 点击点 ({x},{y}) 落在本程序窗口 0x{:X}，取消拾取",
                    hit as isize
                );
                break;
            } else {
                // 点击归位：直接命中候选 / 内层子窗上溯 / 边框环归位
                let root = unsafe { GetAncestor(hit, GA_ROOT) };
                let (h, why) = resolve_click(root, hit, pt);
                if h.is_null() {
                    logline!("[pick] 点击 ({x},{y}) 归位后无窗口，继续拾取");
                    unsafe {
                        ShowWindow(overlay, SW_SHOWNOACTIVATE);
                    }
                } else {
                    logline!(
                        "[pick] 点击 ({x},{y}) 命中 0x{:X} -> 目标 0x{:X}（{}）| {} | 层级：{}",
                        hit as isize,
                        h as isize,
                        why,
                        describe(h).label(),
                        ancestor_chain(h)
                    );
                    if pid_of(h) == me || !looks_pickable(h) {
                        logline!("[pick] 目标不可用（本进程/被过滤），继续拾取");
                        unsafe {
                            ShowWindow(overlay, SW_SHOWNOACTIVATE);
                        }
                    } else {
                        result = Some(h);
                        break;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    result
}

/// 从 `hwnd` 往上到顶层（含）的窗口链，用于诊断"这一下点到了哪一层"。
pub fn ancestor_chain(hwnd: HWND) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut h = hwnd;
    for _ in 0..8 {
        if h.is_null() {
            break;
        }
        let info = describe(h);
        let (w, ht) = info.client_size();
        parts.push(format!(
            "0x{:X} {} {}x{}",
            h as isize,
            if info.class_name.is_empty() {
                "?"
            } else {
                &info.class_name
            },
            w,
            ht
        ));
        let p = unsafe { GetParent(h) };
        if p.is_null() {
            break;
        }
        h = p;
    }
    parts.join(" <- ")
}

/// 这个窗口是否值得作为拾取目标。
///
/// 实测教训：用户点击时光标可能落在**系统浮层**上（例如输入法/文本输入窗口
/// `Windows.UI.Core.CoreWindow`、进程 `textinputhost.exe`，尺寸 318×125），
/// 这类窗口抓帧必然失败，还会把真正的候选顶掉。所以这里主动过滤掉。
fn looks_pickable(h: HWND) -> bool {
    if h.is_null() {
        return false;
    }
    // 隐藏窗口抓不到东西（参考实现里也先做了这一步）
    if unsafe { IsWindowVisible(h) } == 0 {
        return false;
    }
    let info = describe(h);
    let cls = info.class_name.to_lowercase();
    let proc = info.process.to_lowercase();
    if cls.contains("windows.ui.core.corewindow")
        || cls.contains("tooltips_class32")
        || cls == "ime"
        || cls.contains("msctfime")
        || proc.contains("textinputhost")
        || proc.contains("searchhost")
    {
        return false;
    }
    // 太小的窗口不可能是画布
    let (w, ht) = client_size(h);
    w >= 64 && ht >= 64
}

// ---------------------------------------------------------------------------
// 时间戳（会话目录命名与 markers.json 用）
// ---------------------------------------------------------------------------

pub fn local_now() -> SYSTEMTIME {
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut st) };
    st
}

/// 会话目录名用的时间：`2026-02-14_13-20-51`
pub fn stamp_folder(st: &SYSTEMTIME) -> String {
    format!(
        "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    )
}

/// 导出文件名用的时间：`20261018-120530`（紧凑、可排序、可直接做文件名）
pub fn stamp_compact(st: &SYSTEMTIME) -> String {
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    )
}

/// markers.json 里用的时间：`2026-02-14T13:20:51.123`
pub fn stamp_iso(st: &SYSTEMTIME) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond, st.wMilliseconds
    )
}

/// 把用户输入的会话名清成安全文件名
pub fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || r#"\/:*?"<>|"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .take(80)
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').to_string();
    if trimmed.is_empty() {
        return "未命名".to_string();
    }
    // Windows 设备名（CON/NUL/COM1…）不能直接作为文件名。
    let stem = trimmed.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        format!("_{trimmed}")
    } else {
        trimmed
    }
}

/// 取窗口所属进程 ID
pub fn pid_of(hwnd: HWND) -> u32 {
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    pid
}

/// 当前**前台窗口**所属进程 ID（0 = 取不到）。
///
/// 用途（按用户要求）：录制时判断"画家现在是否还在绘画软件里"。
/// 别的软件在前台 => 此刻绘画软件是挂起的、不会产生新笔触，不必抓帧。
/// 注意我们自己的悬浮条带 `WS_EX_NOACTIVATE`，点它不会抢前台，所以不会误触发。
pub fn foreground_pid() -> u32 {
    unsafe {
        let h = GetForegroundWindow();
        if h.is_null() {
            return 0;
        }
        pid_of(h)
    }
}

/// 当前窗口所在显示器的工作区（避开任务栏）。
///
/// `MonitorFromWindow` 对空句柄会返回主显示器；取不到工作区时退回
/// `SPI_GETWORKAREA`，再失败才用 1920×1080。所有需要“贴屏幕边缘”的窗口
/// （悬浮条、合成弹窗）都应使用这个函数，而不是 `SPI_GETWORKAREA` ——
/// 后者永远只返回主屏，副屏上的窗口会被拉回主屏。
pub fn monitor_work_area(hwnd: HWND) -> RECT {
    unsafe {
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if !mon.is_null() {
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(mon, &mut mi) != 0
                && mi.rcWork.right > mi.rcWork.left
                && mi.rcWork.bottom > mi.rcWork.top
            {
                return mi.rcWork;
            }
        }
    }
    let mut r: RECT = unsafe { std::mem::zeroed() };
    let ok = unsafe { SystemParametersInfoW(0x0030, 0, &mut r as *mut _ as *mut _, 0) };
    if ok != 0 && r.right > r.left && r.bottom > r.top {
        r
    } else {
        RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        }
    }
}

/// 窗口所在显示器的 DPI（96 = 100%）。失败时退回系统 DPI。
pub fn dpi_of(hwnd: HWND) -> u32 {
    let dpi = unsafe {
        if hwnd.is_null() {
            0
        } else {
            windows_sys::Win32::UI::HiDpi::GetDpiForWindow(hwnd)
        }
    };
    if dpi > 0 {
        dpi
    } else {
        system_dpi()
    }
}

/// 系统启动时的主屏 DPI（用于窗口创建前的初始尺寸）。
pub fn system_dpi() -> u32 {
    let dpi = unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForSystem() };
    if dpi > 0 {
        dpi
    } else {
        96
    }
}

/// Windows 目录（通常是 `C:\Windows`）。用于拼出系统程序/库的绝对路径，
/// 避免依赖 PATH 或 exe 同目录搜索。
pub fn windows_dir() -> Option<std::path::PathBuf> {
    let mut buf = [0u16; 260];
    let n = unsafe { GetWindowsDirectoryW(buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 || n as usize >= buf.len() {
        return None;
    }
    Some(std::path::PathBuf::from(String::from_utf16_lossy(
        &buf[..n as usize],
    )))
}

/// 查询指定路径所在卷的可用字节数。路径不存在时返回 None。
pub fn disk_free_bytes(path: &std::path::Path) -> Option<u64> {
    let path = path.to_string_lossy();
    let path = wide(&path);
    let mut available: u64 = 0;
    let mut total: u64 = 0;
    let mut free: u64 = 0;
    let ok = unsafe { GetDiskFreeSpaceExW(path.as_ptr(), &mut available, &mut total, &mut free) };
    if ok != 0 {
        Some(available)
    } else {
        None
    }
}

/// 供 capture 模块建宿主窗口用
pub fn module_handle() -> *mut c_void {
    unsafe { GetModuleHandleW(null()) }
}

// ---------------------------------------------------------------------------
// 单实例
// ---------------------------------------------------------------------------
//
// 用**命名互斥体**（`Local\` 前缀 = 当前登录会话）：进程退出或崩溃时内核自动释放。
// 刻意不用"锁文件"方案 —— 崩溃后会留下假锁，用户再也打不开程序（这类坑很常见）。

static INSTANCE_MUTEX: std::sync::OnceLock<isize> = std::sync::OnceLock::new();

/// 尝试取得单实例锁：`true` = 本进程是唯一实例；`false` = 已有实例在跑。
pub fn single_instance_acquire() -> bool {
    let name = wide("Local\\CanLapse.SingleInstance");
    let h = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if h.is_null() {
        // 创建失败（权限/资源极少见）时不要让用户完全打不开程序，放行并记日志
        logline!(
            "[start] 单实例互斥体创建失败（err={}），继续启动",
            unsafe { GetLastError() }
        );
        return true;
    }
    let already = unsafe { GetLastError() } == 183; // ERROR_ALREADY_EXISTS
                                                    // 句柄保持到进程结束（不要 CloseHandle，否则锁会提前释放）
    let _ = INSTANCE_MUTEX.set(h as isize);
    !already
}

// ---------------------------------------------------------------------------
// 子窗口几何：抓"画布子窗口"的关键
// ---------------------------------------------------------------------------

/// 取窗口的**顶层祖先**。DWM 缩略图只能对顶层窗口注册
/// （对子窗口调用 `DwmRegisterThumbnail` 会返回 E_INVALIDARG —— SAI2 上实测）。
pub fn root_of(hwnd: HWND) -> HWND {
    unsafe {
        let r = GetAncestor(hwnd, GA_ROOT);
        if r.is_null() {
            hwnd
        } else {
            r
        }
    }
}

/// 计算 `child` 的**客户区**在 `root` 客户区坐标系里的矩形 `(x, y, w, h)`。
///
/// 抓"画布子窗口"的做法就是：对 `root` 注册缩略图，把这个矩形当作 `rcSource`，
/// DWM 会直接把这块 1:1 送到宿主窗口 —— 既拿到的是子窗口内容，
/// 又完全不走屏幕像素，因此不受遮挡影响。
pub fn crop_rect_in_root(child: HWND, root: HWND) -> Option<(i32, i32, i32, i32)> {
    unsafe {
        let mut rc: RECT = std::mem::zeroed();
        if GetClientRect(child, &mut rc) == 0 {
            return None;
        }
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        if w <= 0 || h <= 0 {
            return None;
        }
        let mut cp = POINT { x: 0, y: 0 };
        if ClientToScreen(child, &mut cp) == 0 {
            return None;
        }
        let mut rp = POINT { x: 0, y: 0 };
        if ClientToScreen(root, &mut rp) == 0 {
            return None;
        }
        Some((cp.x - rp.x, cp.y - rp.y, w, h))
    }
}

/// 光标下"有意义"的窗口。
///
/// `WindowFromPoint` 本身就会返回子窗口（画布常常是子窗口），
/// 但它也可能返回分隔条/滚动条这类极小的控件 —— 那对用户没意义。
/// 所以遇到太小的窗口就往上退一层，最多退几层。
pub fn cursor_window_deep() -> HWND {
    unsafe {
        let mut pt = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut pt) == 0 {
            return null_mut();
        }
        let mut h = WindowFromPoint(pt);
        let me = std::process::id();
        for _ in 0..4 {
            if h.is_null() {
                break;
            }
            let mut rc: RECT = std::mem::zeroed();
            GetWindowRect(h, &mut rc);
            let (w, ht) = (rc.right - rc.left, rc.bottom - rc.top);
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(h, &mut pid);
            if pid == me || w < 24 || ht < 24 {
                let parent = GetParent(h);
                if parent.is_null() {
                    break;
                }
                h = parent;
                continue;
            }
            break;
        }
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 离屏可见比例覆盖窄边和完全离屏() {
        let rc = RECT {
            left: -484,
            top: 358,
            right: 43,
            bottom: 1125,
        };
        let ratio = visible_ratio(&rc, 0, 0, 3600, 1080);
        assert!((ratio - 43.0 / 527.0 * 722.0 / 767.0).abs() < 0.01);
        assert!(ratio < 0.25);

        let outside = RECT {
            left: -600,
            top: 0,
            right: -10,
            bottom: 700,
        };
        assert_eq!(visible_ratio(&outside, 0, 0, 3600, 1080), 0.0);
    }

    /// 点击归位用到的几何判定（纯函数，便于回归）
    #[test]
    fn 点在矩形内与边框环的判定() {
        let rc = RECT {
            left: 100,
            top: 100,
            right: 300,
            bottom: 200,
        };
        // 内部
        assert!(point_in_rect(POINT { x: 200, y: 150 }, &rc));
        assert!(!point_near_edge(POINT { x: 200, y: 150 }, &rc, BORDER_PX));
        // 边界上（右侧）
        assert!(point_near_edge(POINT { x: 299, y: 150 }, &rc, BORDER_PX));
        // 外侧 3px（在环内）
        assert!(point_near_edge(POINT { x: 303, y: 150 }, &rc, BORDER_PX));
        // 外侧 10px（环外）
        assert!(!point_near_edge(POINT { x: 310, y: 150 }, &rc, BORDER_PX));
        // 四角也算边框
        assert!(point_near_edge(POINT { x: 100, y: 100 }, &rc, BORDER_PX));
        // 外部远处
        assert!(!point_in_rect(POINT { x: 10, y: 10 }, &rc));
    }

    #[test]
    fn 可拾取尺寸下限() {
        // 这条只验证常量没被改回 0 —— 真实窗口判定依赖系统，不在单测里造窗口
        const _: () = {
            assert!(CAND_MIN >= 64);
            assert!(BORDER_PX > 0 && BORDER_PX < 20);
        };
    }

    #[test]
    fn 会话名清洗非法字符() {
        assert_eq!(sanitize_name("下雨天/喝:咖啡"), "下雨天_喝_咖啡");
        assert_eq!(sanitize_name("  a<b>c|d  "), "a_b_c_d");
    }

    #[test]
    fn 空名字有兜底() {
        assert_eq!(sanitize_name("   "), "未命名");
        assert_eq!(sanitize_name("..."), "未命名");
    }

    #[test]
    fn 设备名和控制字符被处理() {
        assert_eq!(sanitize_name("CON"), "_CON");
        assert_eq!(sanitize_name("com1.txt"), "_com1.txt");
        assert_eq!(sanitize_name("a\nb"), "a_b");
        assert!(sanitize_name(&"长".repeat(200)).chars().count() <= 80);
    }

    /// 需求「跟随窗口缩放」的几何验证：裁剪矩形必须随子窗口移动/缩放而变。
    /// 用自己创建的父子窗口验证，**不去动用户的绘画软件窗口**。
    #[test]
    fn 子窗口裁剪矩形随子窗口移动与缩放() {
        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, SetWindowPos,
                SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WNDCLASSW, WS_CHILD,
                WS_POPUP, WS_VISIBLE,
            };
            let hinst = module_handle();
            let mk_class = |name: &str| -> Vec<u16> {
                let n = wide(name);
                let wc = WNDCLASSW {
                    style: 0,
                    lpfnWndProc: Some(DefWindowProcW),
                    cbClsExtra: 0,
                    cbWndExtra: 0,
                    hInstance: hinst,
                    hIcon: null_mut(),
                    hCursor: null_mut(),
                    hbrBackground: null_mut(),
                    lpszMenuName: null(),
                    lpszClassName: n.as_ptr(),
                };
                RegisterClassW(&wc);
                n
            };
            let pc = mk_class("CanLapseTestParent");
            let parent = CreateWindowExW(
                0,
                pc.as_ptr(),
                wide("parent").as_ptr(),
                WS_POPUP | WS_VISIBLE,
                100,
                100,
                800,
                600,
                null_mut(),
                null_mut(),
                hinst,
                null(),
            );
            assert!(!parent.is_null(), "创建父窗口失败");
            let cc = mk_class("CanLapseTestChild");
            let child = CreateWindowExW(
                0,
                cc.as_ptr(),
                wide("child").as_ptr(),
                WS_CHILD | WS_VISIBLE,
                50,
                60,
                200,
                150,
                parent,
                null_mut(),
                hinst,
                null(),
            );
            assert!(!child.is_null(), "创建子窗口失败");

            let r = crop_rect_in_root(child, parent).expect("应能算出裁剪矩形");
            assert_eq!((r.0, r.1, r.2, r.3), (50, 60, 200, 150), "初始裁剪矩形");

            // 缩放子窗口 -> 裁剪尺寸跟随
            SetWindowPos(
                child,
                null_mut(),
                0,
                0,
                120,
                90,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let r2 = crop_rect_in_root(child, parent).unwrap();
            assert_eq!((r2.2, r2.3), (120, 90), "裁剪尺寸应跟随子窗口缩放");
            assert_eq!((r2.0, r2.1), (50, 60), "缩放不应改变偏移");

            // 移动子窗口 -> 裁剪偏移跟随
            SetWindowPos(
                child,
                null_mut(),
                30,
                40,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let r3 = crop_rect_in_root(child, parent).unwrap();
            assert_eq!((r3.0, r3.1), (30, 40), "裁剪偏移应跟随子窗口移动");

            // 顶层窗口自己：矩形应等于整个客户区
            let r4 = crop_rect_in_root(parent, parent).unwrap();
            assert_eq!((r4.0, r4.1), (0, 0));
            assert_eq!((r4.2, r4.3), (800, 600));

            DestroyWindow(child);
            DestroyWindow(parent);
        }
    }

    #[test]
    fn 时间戳格式符合目录命名要求() {
        let st = SYSTEMTIME {
            wYear: 2026,
            wMonth: 2,
            wDayOfWeek: 6,
            wDay: 14,
            wHour: 13,
            wMinute: 20,
            wSecond: 51,
            wMilliseconds: 123,
        };
        assert_eq!(stamp_folder(&st), "2026-02-14_13-20-51");
        assert_eq!(stamp_iso(&st), "2026-02-14T13:20:51.123");
    }
}
