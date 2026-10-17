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
    GetForegroundWindow, GetParent, GetWindow, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, MSG, PM_REMOVE, PeekMessageW,
    TranslateMessage, WindowFromPoint, GA_ROOT, GW_OWNER,
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
        let is_child = unsafe { !GetParent(self.hwnd).is_null() };
        format!(
            "{}{} [{}] {}x{}",
            t,
            if is_child { "（子窗口）" } else { "" },
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

/// 拾取时跟随鼠标的提示条（Win32 tooltip）。
///
/// 为什么用 tooltip 而不是自建窗口：tooltip 天生**鼠标穿透、不抢焦点**，
/// 不会挡住用户要点的那条边框 —— 自建窗口必须自己加 WS_EX_TRANSPARENT，容易漏。
pub fn pick_hint_create(owner: HWND, text: &str) -> HWND {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, SendMessageW, WS_POPUP,
    };
    // 自己按 ABI 定义 TOOLINFOW，避免去猜它落在哪个模块
    #[repr(C)]
    struct ToolInfo {
        cb_size: u32,
        flags: u32,
        hwnd: HWND,
        id: usize,
        rect: RECT,
        inst: isize,
        text: *mut u16,
        lparam: isize,
        reserved: *mut core::ffi::c_void,
    }
    unsafe {
        let cls = wide("tooltips_class32");
        // ex: LAYERED(0x80000) | TOPMOST(0x8) | TOOLWINDOW(0x80) | TRANSPARENT(0x20)
        let tip = CreateWindowExW(
            0x0008_0000 | 0x0000_0008 | 0x0000_0080 | 0x0000_0020,
            cls.as_ptr(),
            null(),
            WS_POPUP | 0x0000_0003, // TTS_ALWAYSTIP|TTS_NOPREFIX
            0,
            0,
            0,
            0,
            owner,
            null_mut(),
            module_handle(),
            null(),
        );
        if tip.is_null() {
            return tip;
        }
        let mut t: Vec<u16> = text.encode_utf16().collect();
        t.push(0);
        let mut ti: ToolInfo = std::mem::zeroed();
        ti.cb_size = std::mem::size_of::<ToolInfo>() as u32;
        ti.flags = 0x0020 | 0x0080; // TTF_TRACK | TTF_ABSOLUTE
        ti.hwnd = owner;
        ti.id = 1;
        ti.text = t.as_mut_ptr();
        SendMessageW(tip, 0x0432, 0, &ti as *const ToolInfo as isize); // TTM_ADDTOOLW
        SendMessageW(tip, 0x0411, 1, &ti as *const ToolInfo as isize); // TTM_TRACKACTIVATE
        let p = cursor_position();
        SendMessageW(tip, 0x0412, 0, pack_xy(p.0 + 18, p.1 + 22)); // TTM_TRACKPOSITION
        tip
    }
}

/// 让提示条跟随鼠标（拾取循环里每次迭代调用）
pub fn pick_hint_follow(tip: HWND) {
    if tip.is_null() {
        return;
    }
    let p = cursor_position();
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW(
            tip,
            0x0412,
            0,
            pack_xy(p.0 + 18, p.1 + 22),
        );
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

fn pack_xy(x: i32, y: i32) -> isize {
    ((y as isize) << 16) | (x as isize & 0xffff)
}

fn cursor_position() -> (i32, i32) {
    unsafe {
        let mut p: POINT = std::mem::zeroed();
        GetCursorPos(&mut p);
        (p.x, p.y)
    }
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
    // 注意：**不要**上溯到顶层窗口。SAI2 的画布就是子窗口，
    // 上溯会把整个主窗口（含所有面板）当成目标 —— 录出来是一堆工具栏。
    // 拾取要取光标下最深的那一层。
    cursor_window_deep()
}

/// 交互式拾取：鼠标移到目标窗口上，实时打印光标下的窗口，点左键确认，Esc 取消。
///
/// v1.0 的 GUI 版会把"实时打印"换成屏幕高亮框；选择逻辑本身是同一套。
pub fn pick_window_interactive(hint: Option<HWND>) -> Option<HWND> {
    let me = std::process::id();
    let mut last: isize = 0;
    // 记住"最后一个有效的非本进程窗口"。
    // 旧实现把点击判定放在 pid/null 检查之后：只要鼠标当下落在我们自己的窗口或空处，
    // 代码就 continue，**永远走不到检测点击那一步** —— 表现为"最小化后点哪都没反应"。
    let mut candidate: HWND = null_mut();
    let mut ticks = 0u32;
    loop {
        pump();
        if key_down(VK_ESCAPE) {
            return None;
        }
        let h = cursor_window();
        if let Some(tip) = hint {
            pick_hint_follow(tip);
        }
        if !h.is_null() && pid_of(h) != me && looks_pickable(h) {
            candidate = h;
            if h as isize != last {
                last = h as isize;
                println!("  当前指向: {}", describe(h).label());
            }
        }
        // 诊断日志（最多 300 行）：下一次失败时可以直接看出卡在哪一步
        if ticks < 300 {
            writes_pick_log(&format!(
                "t{ticks} cursor=0x{:X} pid={} cand=0x{:X} lbtn={}",
                h as isize,
                if h.is_null() { 0 } else { pid_of(h) },
                candidate as isize,
                key_down(VK_LBUTTON)
            ));
            ticks += 1;
        }
        if key_down(VK_LBUTTON) {
            // 等左键松开，避免把这次按下当成"开始录制"之类
            while key_down(VK_LBUTTON) {
                pump();
                std::thread::sleep(Duration::from_millis(10));
            }
            if !candidate.is_null() {
                return Some(candidate);
            }
        }
        std::thread::sleep(Duration::from_millis(60));
    }
}

/// 把拾取过程的诊断写进 %LOCALAPPDATA%\CanLapse\pick.log（追加）。
/// 这个窗口是否值得作为拾取目标。
///
/// 实测教训：用户点击时光标可能落在**系统浮层**上（例如输入法/文本输入窗口
/// `Windows.UI.Core.CoreWindow`、进程 `textinputhost.exe`，尺寸 318×125），
/// 这类窗口抓帧必然失败，还会把真正的候选顶掉。所以这里主动过滤掉。
fn looks_pickable(h: HWND) -> bool {
    if h.is_null() {
        return false;
    }
    let info = describe(h);
    let cls = info.class_name.to_lowercase();
    let proc = info.process.to_lowercase();
    if cls.contains("windows.ui.core.corewindow")
        || cls.contains("tooltips_class32")
        || cls.contains("ime")
        || proc.contains("textinputhost")
        || proc.contains("searchhost")
    {
        return false;
    }
    // 太小的窗口不可能是画布
    let (w, ht) = client_size(h);
    w >= 64 && ht >= 64
}

fn writes_pick_log(line: &str) {
    use std::io::Write;
    // 优先用 CANLAPSE_LOG 指定的文件（测试时写进工作区，保证可读）；
    // 否则退回 %LOCALAPPDATA%\CanLapse\pick.log
    let path = if let Ok(p) = std::env::var("CANLAPSE_LOG") {
        std::path::PathBuf::from(p)
    } else {
        let dir = std::env::var("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join("CanLapse");
        let _ = std::fs::create_dir_all(&dir);
        dir.join("pick.log")
    };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
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

/// 统计画布区域被"**同属主**的其他窗口"遮住的比例（0.0~1.0）。
///
/// 为什么需要它 —— 这是实测得出的重要事实：
/// * DWM 缩略图会把**被该窗口拥有（owned）的窗口一起渲染**。SAI2 的浮动面板
///   （「颜色」等）的 `GW_OWNER` 就是主窗口，所以它们**会出现在画布里**；
///   而别的软件的窗口不属于这个属主组，因此不会串进来 ——
///   用户观察到的"自家面板能录进、别家窗口录不进"正是这个原因。
/// * 被面板盖住的那部分画布像素，应用**根本没有绘制到任何地方**，
///   所以没有任何抓帧方式能把它拿回来。产品能做的是**如实提醒用户移开面板**，
///   而不是假装能录到。
///
/// `crop` 是相对 `root` 客户区原点的矩形 `(x, y, w, h)`。
pub fn covered_ratio(root: HWND, crop: (i32, i32, i32, i32)) -> f32 {
    let (cx, cy, cw, ch) = crop;
    if cw <= 0 || ch <= 0 {
        return 0.0;
    }
    // 用一个粗网格做并集，避免多块面板互相重叠时把面积重复计算
    const GRID: i32 = 64;
    let mut cells = vec![false; (GRID * GRID) as usize];
    unsafe {
        let mut owned: Vec<RECT> = Vec::new();
        let root_for_cb = root;
        unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let out = &mut *(lparam as *mut (HWND, Vec<RECT>));
            let (root, list) = out;
            if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 {
                return 1;
            }
            // 只看"被 root 拥有"的顶层窗口（GetWindow(GW_OWNER)）
            let owner = GetWindow(hwnd, GW_OWNER);
            if owner != *root {
                return 1;
            }
            let mut rc: RECT = std::mem::zeroed();
            if GetWindowRect(hwnd, &mut rc) == 0 {
                return 1;
            }
            // 转成 root 客户区坐标
            let mut tl = POINT { x: rc.left, y: rc.top };
            let mut br = POINT { x: rc.right, y: rc.bottom };
            windows_sys::Win32::Graphics::Gdi::ScreenToClient(*root, &mut tl);
            windows_sys::Win32::Graphics::Gdi::ScreenToClient(*root, &mut br);
            list.push(RECT { left: tl.x, top: tl.y, right: br.x, bottom: br.y });
            1
        }
        let mut state = (root_for_cb, owned);
        EnumWindows(
            Some(cb),
            &mut state as *mut (HWND, Vec<RECT>) as LPARAM,
        );
        owned = state.1;

        for rc in &owned {
            // 与裁剪矩形求交
            let l = rc.left.max(cx);
            let t = rc.top.max(cy);
            let r = rc.right.min(cx + cw);
            let b = rc.bottom.min(cy + ch);
            if r <= l || b <= t {
                continue;
            }
            // 映射到网格
            let gx0 = ((l - cx) as i64 * GRID as i64 / cw as i64).clamp(0, GRID as i64 - 1) as i32;
            let gx1 = ((r - cx) as i64 * GRID as i64 / cw as i64).clamp(0, GRID as i64) as i32;
            let gy0 = ((t - cy) as i64 * GRID as i64 / ch as i64).clamp(0, GRID as i64 - 1) as i32;
            let gy1 = ((b - cy) as i64 * GRID as i64 / ch as i64).clamp(0, GRID as i64) as i32;
            for gy in gy0..gy1 {
                for gx in gx0..gx1 {
                    cells[(gy * GRID + gx) as usize] = true;
                }
            }
        }
    }
    let hit = cells.iter().filter(|c| **c).count();
    hit as f32 / (GRID * GRID) as f32
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

/// 供 capture 模块建宿主窗口用
pub fn module_handle() -> *mut c_void {
    unsafe { GetModuleHandleW(null()) }
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
    fn 会话名清洗非法字符() {
        assert_eq!(sanitize_name("下雨天/喝:咖啡"), "下雨天_喝_咖啡");
        assert_eq!(sanitize_name("  a<b>c|d  "), "a_b_c_d");
    }

    #[test]
    fn 空名字有兜底() {
        assert_eq!(sanitize_name("   "), "未命名");
        assert_eq!(sanitize_name("..."), "未命名");
    }

    /// 「画布被自家面板遮挡」的检测：属主窗口覆盖面积比例。
    /// 实测背景：DWM 缩略图会连同**被拥有（owned）的窗口**一起渲染，
    /// 所以 SAI2 的浮动面板会出现在画布里；被盖住的像素应用根本没画，抓不回来 ——
    /// 产品能做的是如实提醒，这个函数就是提醒的依据。
    #[test]
    fn 属主窗口遮挡比例可计算() {
        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, SetWindowPos,
                SWP_NOACTIVATE, SWP_NOZORDER, WNDCLASSW, WS_CHILD, WS_POPUP, WS_VISIBLE,
            };
            let hinst = module_handle();
            let mk = |n: &str| -> Vec<u16> {
                let w = wide(n);
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
                    lpszClassName: w.as_ptr(),
                };
                RegisterClassW(&wc);
                w
            };
            let pc = mk("CanLapseCovRoot");
            let root = CreateWindowExW(
                0, pc.as_ptr(), wide("root").as_ptr(), WS_POPUP | WS_VISIBLE,
                100, 100, 800, 600, null_mut(), null_mut(), hinst, null(),
            );
            let cc = mk("CanLapseCovChild");
            let child = CreateWindowExW(
                0, cc.as_ptr(), wide("child").as_ptr(), WS_CHILD | WS_VISIBLE,
                50, 60, 400, 400, root, null_mut(), hinst, null(),
            );
            // 被 root 拥有的弹窗，盖在 child 左上 100x200 处
            let oc = mk("CanLapseCovOwned");
            let owned = CreateWindowExW(
                0, oc.as_ptr(), wide("owned").as_ptr(), WS_POPUP | WS_VISIBLE,
                150, 160, 100, 200, root, null_mut(), hinst, null(),
            );
            SetWindowPos(
                owned, null_mut(), 150, 160, 100, 200,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let crop = crop_rect_in_root(child, root).unwrap();
            let ratio = covered_ratio(root, crop);
            // 100x200 / (400x400) = 12.5%
            assert!(
                (ratio - 0.125).abs() < 0.02,
                "遮挡比例应约 12.5%，实测 {ratio}"
            );
            DestroyWindow(owned);
            DestroyWindow(child);
            DestroyWindow(root);
        }
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
