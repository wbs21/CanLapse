//! Win32 基础封装：窗口枚举、进程名、鼠标拾取、消息泵、本地时间。

use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, POINT, RECT, SYSTEMTIME};
// 注意：ClientToScreen 在 Graphics::Gdi 下，不在 WindowsAndMessaging（实测踩过）。
use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_ESCAPE, VK_F8, VK_LBUTTON,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, EnumWindows, GetAncestor, GetClassNameW, GetClientRect, GetCursorPos,
    GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible,
    MSG, PM_REMOVE, PeekMessageW, TranslateMessage, WindowFromPoint, GA_ROOT,
};

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
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
        format!(
            "{} [{}] {}x{}",
            t,
            if self.process.is_empty() { "?" } else { &self.process },
            self.client.right - self.client.left,
            self.client.bottom - self.client.top
        )
    }

    pub fn client_size(&self) -> (i32, i32) {
        (self.client.right - self.client.left, self.client.bottom - self.client.top)
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
    if IsWindowVisible(hwnd) == 0 {
        return 1;
    }
    let out = &mut *(lparam as *mut Vec<WindowInfo>);
    let info = describe(hwnd);
    if info.title.is_empty() && info.class_name.is_empty() {
        return 1;
    }
    out.push(info);
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

/// 按标题子串 / 类名 / 进程名查找（用于命令行指定目标）
pub fn find_window(spec: &str) -> Option<WindowInfo> {
    let low = spec.to_lowercase();
    list_windows()
        .into_iter()
        .find(|w| {
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

fn key_down(vk: u16) -> bool {
    unsafe { (GetAsyncKeyState(vk as i32) as u16) & 0x8000 != 0 }
}

/// 录制循环里用：是否按下了 Esc（请求停止录制）
pub fn escape_pressed() -> bool {
    key_down(VK_ESCAPE)
}

static MARK_KEY_DOWN: AtomicBool = AtomicBool::new(false);

/// 录制循环里用：F8 是否**刚被按下**（边沿触发，按住不会连打标记）。
///
/// 规格里的 🏷 标记按钮在悬浮操作条上；命令行切片阶段用 F8 作为等价入口。
pub fn marker_key_pressed() -> bool {
    let down = key_down(VK_F8);
    let was = MARK_KEY_DOWN.swap(down, Ordering::SeqCst);
    down && !was
}

fn cursor_window() -> HWND {
    unsafe {
        let mut pt = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut pt) == 0 {
            return null_mut();
        }
        let h = WindowFromPoint(pt);
        if h.is_null() {
            return null_mut();
        }
        // 点到的是子窗口（画布常常是子窗口），要上溯到顶层窗口
        let root = GetAncestor(h, GA_ROOT);
        if root.is_null() {
            h
        } else {
            root
        }
    }
}

/// 交互式拾取：鼠标移到目标窗口上，实时打印光标下的窗口，点左键确认，Esc 取消。
///
/// v1.0 的 GUI 版会把"实时打印"换成屏幕高亮框；选择逻辑本身是同一套。
pub fn pick_window_interactive() -> Option<HWND> {
    println!("把鼠标移到目标画布窗口上，按一下【左键】确认；按【Esc】取消。");
    let me = std::process::id();
    let mut last: isize = 0;
    loop {
        pump();
        if key_down(VK_ESCAPE) {
            println!("\n已取消拾取。");
            return None;
        }
        let h = cursor_window();
        if h.is_null() {
            continue;
        }
        let info = describe(h);
        if info.pid == me {
            // 别把自己（控制台窗口）选进来
            continue;
        }
        if h as isize != last {
            last = h as isize;
            println!("  当前指向: {}", info.label());
        }
        if key_down(VK_LBUTTON) {
            // 等左键松开，避免把这次按下当成"开始录制"之类
            while key_down(VK_LBUTTON) {
                pump();
                std::thread::sleep(Duration::from_millis(10));
            }
            return Some(h);
        }
        std::thread::sleep(Duration::from_millis(60));
    }
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
        .map(|c| if r#"\/:*?"<>|"#.contains(c) { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').to_string();
    if trimmed.is_empty() {
        "未命名".to_string()
    } else {
        trimmed
    }
}

/// 供 capture 模块建宿主窗口用
pub fn module_handle() -> *mut c_void {
    unsafe { GetModuleHandleW(null()) }
}

#[cfg(test)]
mod tests {
    use super::*;

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
