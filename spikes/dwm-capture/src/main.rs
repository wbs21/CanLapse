//! CanLapse · DWM 缩略图抓帧可行性原型
//!
//! 目的：回答一个**只能靠实测回答**的问题 ——
//!   「DWM 窗口缩略图（DwmRegisterThumbnail）到底能不能被程序读到像素？」
//!
//! 背景：`DwmRegisterThumbnail` 只负责把源窗口的实时缩略图**合成**到目标窗口上，
//!   Win32 并没有提供"取缩略图像素"的 API。窗口的 DC（GetDC/BitBlt）拿到的通常是
//!   窗口自己绘制的内容，**不包含** DWM 合成上去的缩略图。所以必须实测这些读回方式：
//!
//!   | 编号 | 方式                          | 说明 |
//!   |------|-------------------------------|------|
//!   | M1   | screen_bitblt                 | 屏幕 BitBlt（被遮挡时应失败，作为对照基线）|
//!   | M2   | printwindow_fullcontent       | 直接对源窗口 PrintWindow(PW_RENDERFULLCONTENT) |
//!   | M3   | printwindow_plain             | 直接对源窗口 PrintWindow(0) |
//!   | M4   | host_printwindow_fullcontent  | 缩略图宿主窗口 + PrintWindow(PW_RENDERFULLCONTENT) |
//!   | M5   | host_bitblt                   | 缩略图宿主窗口 + BitBlt(GetDC(host)) |
//!   | M6   | host_offscreen_printwindow    | 宿主移出屏幕后 + PrintWindow(PW_RENDERFULLCONTENT) |
//!   | M7   | host_screen_bitblt            | 屏幕 BitBlt(宿主窗口矩形)：判断缩略图是否真被合成 |
//!
//! 判定是**自证**的：原型自建一个"源窗口"，按 tick 画确定性图案
//! （四象限纯色 + 16 位二进制计数条 + 移动方块）。抓到帧后逐项核对像素
//! 是否与预期 tick 的图案一致 —— 这样不仅能判断"是不是黑屏"，
//! 还能判断"抓到的到底是不是这个窗口的内容"。再把另一个窗口盖到源窗口上，
//! 对比遮挡前后的结果，得出「被遮挡时能否继续录制」这个产品级结论。
//!
//! 用法：
//!   dwm_capture_spike              自检（自建源窗口 + 遮挡窗口，全方式对比）
//!   dwm_capture_spike --list       列出当前所有可见顶层窗口（为拾取窗口做准备）
//!   dwm_capture_spike --hwnd 0x1234 [--out DIR]   对真实窗口跑全部读回方式
//!   dwm_capture_spike --title 记事本 [--out DIR]   按标题子串找窗口

#![allow(non_snake_case)]

use std::ffi::c_void;
use std::fs::File;
use std::io::BufWriter;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{CloseHandle, COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DwmIsCompositionEnabled, DwmRegisterThumbnail, DwmUnregisterThumbnail,
    DwmUpdateThumbnailProperties, DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY,
    DWM_TNP_RECTDESTINATION, DWM_TNP_RECTSOURCE, DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
};
// 注意：windows-sys 把不少 user32 的"画图相关"函数放在 Graphics::Gdi 下
// （BeginPaint/EndPaint/FillRect/ClientToScreen/InvalidateRect/UpdateWindow 都是），
// 而 PrintWindow 被按元数据归到了 Storage::Xps —— 都是实测确认过的，别按直觉猜。
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, ClientToScreen, CreateCompatibleDC, CreateDIBSection, CreateSolidBrush,
    DeleteDC, DeleteObject, EndPaint, FillRect, InvalidateRect, SelectObject, UpdateWindow,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBRUSH, HDC, HGDIOBJ, PAINTSTRUCT,
    RGBQUAD, SRCCOPY,
};
use windows_sys::Win32::Storage::Xps::PrintWindow;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EnumWindows, GetClassNameW,
    GetClientRect, GetSystemMetrics, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId,
    IsIconic, IsWindow, IsWindowVisible, MSG, PM_REMOVE, PW_RENDERFULLCONTENT, PeekMessageW, PostMessageW,
    RegisterClassW, SetForegroundWindow, SetTimer, SetWindowPos, SetWindowTextW, ShowWindow,
    TranslateMessage, HWND_TOPMOST, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, SW_SHOWNORMAL, SWP_NOACTIVATE, SWP_SHOWWINDOW, WM_DESTROY, WM_ERASEBKGND,
    WM_PAINT, WM_TIMER, WNDCLASSW, WS_POPUP, WS_VISIBLE,
};

// ============================================================================
// 源窗口上画的确定性图案（抓帧后据此自证内容正确）
// ============================================================================

const TW: i32 = 480; // 源窗口客户端宽
const TH: i32 = 320; // 源窗口客户端高

const BG: [u8; 3] = [50, 50, 50]; // 背景深灰
const SEP: [u8; 3] = [90, 90, 90]; // 分隔条

/// 四象限：(left, top, right, bottom, 颜色)
const QUADS: [(i32, i32, i32, i32, [u8; 3]); 4] = [
    (20, 110, 220, 190, [255, 0, 0]),   // 左上 纯红
    (260, 110, 460, 190, [0, 255, 0]),  // 右上 纯绿
    (20, 230, 220, 300, [0, 0, 255]),   // 左下 纯蓝
    (260, 230, 460, 300, [255, 255, 255]), // 右下 纯白
];
/// 采样中心点，必须落在上面四个矩形内部
const Q_SAMPLE: [(i32, i32); 4] = [(120, 150), (360, 150), (120, 265), (360, 265)];
const Q_HALF: i32 = 20; // 采样半径 -> 40x40 采样块
const Q_TOL: i32 = 24; // 单通道容差

/// 16 位二进制计数条：按 tick 的二进制位画黑白格子
const STRIP_X0: i32 = 4;
const STRIP_Y0: i32 = 0;
const STRIP_CELL: i32 = 24;
const STRIP_N: u32 = 16;

/// 移动方块（黄色）
const BLOCK_X0: i32 = 20;
const BLOCK_Y: i32 = 40;
const BLOCK_W: i32 = 40;
const BLOCK_H: i32 = 40;
const BLOCK_STEP: i32 = 40;
const BLOCK_POSITIONS: u32 = 10;

/// tick 由定时器驱动；FROZEN 时定住不动，保证"预期图案"与"抓到的图案"可比对
static TICK: AtomicU32 = AtomicU32::new(0);
static FROZEN: AtomicBool = AtomicBool::new(false);

unsafe fn fill(hdc: HDC, l: i32, t: i32, r: i32, b: i32, rgb: [u8; 3]) {
    let brush: HBRUSH = CreateSolidBrush(colorref(rgb));
    let mut rc = RECT { left: l, top: t, right: r, bottom: b };
    FillRect(hdc, &rc, brush);
    DeleteObject(brush as HGDIOBJ);
}

fn colorref(rgb: [u8; 3]) -> COLORREF {
    (rgb[2] as u32) << 16 | (rgb[1] as u32) << 8 | rgb[0] as u32
}

fn block_x(tick: u32) -> i32 {
    BLOCK_X0 + (tick % BLOCK_POSITIONS) as i32 * BLOCK_STEP
}

unsafe fn paint_pattern(hdc: HDC, tick: u32, w: i32, h: i32) {
    fill(hdc, 0, 0, w, h, BG);
    // 分隔条只画在图案带以下，避免切断二进制计数条
    fill(hdc, 220, 100, 260, TH, SEP);
    // 二进制计数条
    for i in 0..STRIP_N {
        let on = (tick >> i) & 1 == 1;
        let x = STRIP_X0 + i as i32 * STRIP_CELL;
        let c = if on { [255, 255, 255] } else { [0, 0, 0] };
        fill(hdc, x, STRIP_Y0, x + STRIP_CELL, STRIP_Y0 + STRIP_CELL, c);
    }
    // 移动方块
    let bx = block_x(tick);
    fill(hdc, bx, BLOCK_Y, bx + BLOCK_W, BLOCK_Y + BLOCK_H, [255, 255, 0]);
    // 四象限
    for (l, t, r, b, c) in QUADS {
        fill(hdc, l, t, r, b, c);
    }
    // 右下角标记（品红）。用途：验证**整幅画面**都是 1:1，
    // 而不只是左上角那一小块 —— 万一缩略图被压缩到左上角，
    // 只看左上角的图案仍可能"看起来是对的"。
    if w > 120 && h > 120 {
        fill(hdc, w - 60, h - 60, w - 20, h - 20, [255, 0, 255]);
    }
}

/// 检查右下角标记（用于证明整幅 1:1）
fn corner_marker_ok(cap: &Cap, w: i32, h: i32) -> bool {
    if w <= 120 || h <= 120 {
        return true; // 尺寸太小不画角标，视为不适用
    }
    match cap.get(w - 40, h - 40) {
        Some(p) => close_enough(p, [255, 0, 255], Q_TOL),
        None => false,
    }
}

// ============================================================================
// 窗口过程
// ============================================================================

unsafe extern "system" fn wndproc_target(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut rc);
            paint_pattern(hdc, TICK.load(Ordering::SeqCst), rc.right, rc.bottom);
            EndPaint(hwnd, &ps);
            0
        }
        WM_ERASEBKGND => 1, // 全部在 WM_PAINT 里画，避免闪烁
        WM_TIMER => {
            if !FROZEN.load(Ordering::SeqCst) {
                TICK.fetch_add(1, Ordering::SeqCst);
            }
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_DESTROY => 0,
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 遮挡窗口：整块不透明深黑，用来盖住源窗口
unsafe extern "system" fn wndproc_occluder(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut rc);
            fill(hdc, 0, 0, rc.right, rc.bottom, [0x18, 0x18, 0x18]);
            EndPaint(hwnd, &ps);
            0
        }
        WM_ERASEBKGND => 1,
        WM_DESTROY => 0,
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 缩略图宿主窗口：自己画品红底色。
/// 于是屏幕上看这块区域时：品红 = 缩略图没合成上来；源窗口画面 = 合成成功。
unsafe extern "system" fn wndproc_host(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut rc);
            fill(hdc, 0, 0, rc.right, rc.bottom, [255, 0, 255]);
            EndPaint(hwnd, &ps);
            0
        }
        WM_ERASEBKGND => 1,
        WM_DESTROY => 0,
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// ---- 跨进程验证用的"对端"窗口 -------------------------------------------------
// 关键：它在**另一个进程**里画同样的确定性图案，并把当前 tick 写进窗口标题。
// 主进程于是能跨进程读到"预期 tick"，从而在跨进程条件下也能严格自证抓帧内容。
const WM_APP_SET_TICK: u32 = 0x8000 + 3; // wparam = 目标 tick，并冻结
static HELPER_QUIT: AtomicBool = AtomicBool::new(false);

unsafe fn helper_sync_title(hwnd: HWND) {
    let t = wide(&format!("CanLapseSpikeHelper tick={}", TICK.load(Ordering::SeqCst)));
    SetWindowTextW(hwnd, t.as_ptr());
}

unsafe extern "system" fn wndproc_helper(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut rc);
            paint_pattern(hdc, TICK.load(Ordering::SeqCst), rc.right, rc.bottom);
            EndPaint(hwnd, &ps);
            0
        }
        WM_ERASEBKGND => 1,
        WM_TIMER => {
            if !FROZEN.load(Ordering::SeqCst) {
                TICK.fetch_add(1, Ordering::SeqCst);
                helper_sync_title(hwnd);
            }
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_APP_SET_TICK => {
            FROZEN.store(true, Ordering::SeqCst);
            TICK.store(wparam as u32, Ordering::SeqCst);
            helper_sync_title(hwnd);
            InvalidateRect(hwnd, null(), 1);
            0
        }
        WM_DESTROY => {
            HELPER_QUIT.store(true, Ordering::SeqCst);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// ============================================================================
// 抓帧基础设施
// ============================================================================

/// 一帧 BGRA（top-down，行序从上到下）
struct Cap {
    w: i32,
    h: i32,
    bgra: Vec<u8>,
}

impl Cap {
    fn get(&self, x: i32, y: i32) -> Option<[u8; 3]> {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return None;
        }
        let i = ((y * self.w + x) * 4) as usize;
        // 内存里是 B,G,R,A -> 返回 R,G,B
        Some([self.bgra[i + 2], self.bgra[i + 1], self.bgra[i]])
    }

    fn sub(&self, x0: i32, y0: i32, w: i32, h: i32) -> Cap {
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            let sy = y0 + y;
            let sx = x0;
            if sy < 0 || sy >= self.h || sx < 0 || sx + w > self.w {
                out.resize(out.len() + (w * 4) as usize, 0);
                continue;
            }
            let start = ((sy * self.w + sx) * 4) as usize;
            out.extend_from_slice(&self.bgra[start..start + (w * 4) as usize]);
        }
        Cap { w, h, bgra: out }
    }

    fn save_png(&self, path: &str) -> std::io::Result<()> {
        let mut rgba = Vec::with_capacity(self.bgra.len());
        for px in self.bgra.chunks_exact(4) {
            rgba.push(px[2]);
            rgba.push(px[1]);
            rgba.push(px[0]);
            rgba.push(255);
        }
        let f = File::create(path)?;
        let w = BufWriter::new(f);
        let mut enc = png::Encoder::new(w, self.w as u32, self.h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header()?;
        writer.write_image_data(&rgba)?;
        writer.finish()?;
        Ok(())
    }
}

/// 建一块 32bpp top-down DIB，交给 `f` 去往上面画，然后整块拷出来。
unsafe fn with_dib<F>(w: i32, h: i32, f: F) -> Result<Cap, String>
where
    F: FnOnce(HDC) -> i32,
{
    if w <= 0 || h <= 0 {
        return Err(format!("尺寸非法 {w}x{h}"));
    }
    let screen = windows_sys::Win32::Graphics::Gdi::GetDC(null_mut());
    if screen.is_null() {
        return Err("GetDC(NULL) 失败".into());
    }
    let mem = CreateCompatibleDC(screen);
    if mem.is_null() {
        windows_sys::Win32::Graphics::Gdi::ReleaseDC(null_mut(), screen);
        return Err("CreateCompatibleDC 失败".into());
    }
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // 负数 = top-down，内存行序与直觉一致
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
    let mut bits: *mut c_void = null_mut();
    let hbm = CreateDIBSection(screen, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    if hbm.is_null() || bits.is_null() {
        DeleteDC(mem);
        windows_sys::Win32::Graphics::Gdi::ReleaseDC(null_mut(), screen);
        return Err("CreateDIBSection 失败".into());
    }
    let old = SelectObject(mem, hbm as HGDIOBJ);

    let ok = f(mem);

    let data = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec();
    SelectObject(mem, old);
    DeleteObject(hbm as HGDIOBJ);
    DeleteDC(mem);
    windows_sys::Win32::Graphics::Gdi::ReleaseDC(null_mut(), screen);

    if ok == 0 {
        return Err("绘制调用返回失败(0)".into());
    }
    Ok(Cap { w, h, bgra: data })
}

/// 客户端区域在屏幕上的矩形
unsafe fn client_screen_rect(hwnd: HWND) -> RECT {
    let mut rc: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut rc);
    let mut pt = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
    ClientToScreen(hwnd, &mut pt);
    RECT {
        left: pt.x,
        top: pt.y,
        right: pt.x + (rc.right - rc.left),
        bottom: pt.y + (rc.bottom - rc.top),
    }
}

/// M1 / M7：屏幕 BitBlt 一块屏幕矩形
unsafe fn capture_screen_rect(rc: &RECT) -> Result<Cap, String> {
    let w = rc.right - rc.left;
    let h = rc.bottom - rc.top;
    with_dib(w, h, |mem| {
        let screen = windows_sys::Win32::Graphics::Gdi::GetDC(null_mut());
        let ok = BitBlt(mem, 0, 0, w, h, screen, rc.left, rc.top, SRCCOPY);
        windows_sys::Win32::Graphics::Gdi::ReleaseDC(null_mut(), screen);
        ok
    })
}

/// M2 / M3 / M4 / M6：PrintWindow 抓窗口，然后裁到客户端区域
unsafe fn capture_printwindow(hwnd: HWND, flags: u32) -> Result<Cap, String> {
    if IsWindow(hwnd) == 0 {
        return Err("hwnd 无效".into());
    }
    let mut wr: RECT = std::mem::zeroed();
    GetWindowRect(hwnd, &mut wr);
    let ww = wr.right - wr.left;
    let wh = wr.bottom - wr.top;
    let full = with_dib(ww, wh, |mem| PrintWindow(hwnd, mem, flags))?;

    // 裁到客户端区域：PrintWindow 画的是整个窗口（含标题栏/边框）
    let mut rc: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut rc);
    let mut pt = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
    ClientToScreen(hwnd, &mut pt);
    let off_x = pt.x - wr.left;
    let off_y = pt.y - wr.top;
    Ok(full.sub(off_x, off_y, rc.right - rc.left, rc.bottom - rc.top))
}

/// M5：BitBlt 窗口自己的 DC
unsafe fn capture_window_dc(hwnd: HWND) -> Result<Cap, String> {
    if IsWindow(hwnd) == 0 {
        return Err("hwnd 无效".into());
    }
    let mut rc: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut rc);
    let w = rc.right - rc.left;
    let h = rc.bottom - rc.top;
    with_dib(w, h, |mem| {
        let dc = windows_sys::Win32::Graphics::Gdi::GetDC(hwnd);
        if dc.is_null() {
            return 0;
        }
        let ok = BitBlt(mem, 0, 0, w, h, dc, 0, 0, SRCCOPY);
        windows_sys::Win32::Graphics::Gdi::ReleaseDC(hwnd, dc);
        ok
    })
}

// ============================================================================
// 内容自证：核对抓到的像素是不是"预期 tick 的图案"
// ============================================================================

fn close_enough(p: [u8; 3], want: [u8; 3], tol: i32) -> bool {
    (p[0] as i32 - want[0] as i32).abs() <= tol
        && (p[1] as i32 - want[1] as i32).abs() <= tol
        && (p[2] as i32 - want[2] as i32).abs() <= tol
}

/// 四象限纯色命中率（1.0 = 完全吻合）
fn quadrant_score(cap: &Cap) -> f32 {
    let mut total = 0.0f32;
    for (i, (cx, cy)) in Q_SAMPLE.iter().enumerate() {
        let want = QUADS[i].4;
        let (mut ok, mut n) = (0i32, 0i32);
        for dy in -Q_HALF..Q_HALF {
            for dx in -Q_HALF..Q_HALF {
                if let Some(p) = cap.get(cx + dx, cy + dy) {
                    n += 1;
                    if close_enough(p, want, Q_TOL) {
                        ok += 1;
                    }
                }
            }
        }
        total += ok as f32 / n.max(1) as f32;
    }
    total / 4.0
}

/// 读二进制计数条；有任何一格既不像黑也不像白就返回 None
fn bit_value(cap: &Cap) -> Option<u32> {
    let mut v = 0u32;
    for i in 0..STRIP_N {
        let x = STRIP_X0 + i as i32 * STRIP_CELL + STRIP_CELL / 2;
        let y = STRIP_Y0 + STRIP_CELL / 2;
        let p = cap.get(x, y)?;
        let lum = (p[0] as u32 + p[1] as u32 + p[2] as u32) / 3;
        if lum > 200 {
            v |= 1 << i;
        } else if lum >= 60 {
            return None; // 灰阶 -> 既非黑也非白，说明内容不是我们的图案
        }
    }
    Some(v)
}

struct Stats {
    distinct: usize,
    mean: f32,
    min: u8,
    max: u8,
}

fn stats(cap: &Cap) -> Stats {
    use std::collections::HashSet;
    let mut set: HashSet<u32> = HashSet::new();
    let (mut sum, mut n, mut min, mut max) = (0f64, 0u64, 255u8, 0u8);
    let mut y = 0;
    while y < cap.h {
        let mut x = 0;
        while x < cap.w {
            if let Some(p) = cap.get(x, y) {
                let key = (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32;
                set.insert(key);
                let lum = ((p[0] as u32 + p[1] as u32 + p[2] as u32) / 3) as u8;
                sum += lum as f64;
                n += 1;
                min = min.min(lum);
                max = max.max(lum);
            }
            x += 4;
        }
        y += 4;
    }
    Stats {
        distinct: set.len(),
        mean: if n > 0 { (sum / n as f64) as f32 } else { 0.0 },
        min,
        max,
    }
}

/// 两帧的差异像素占比（逐像素三通道差之和 > 24 记为不同）
fn diff_ratio(a: &Cap, b: &Cap) -> f32 {
    if a.w != b.w || a.h != b.h {
        return -1.0;
    }
    let (mut d, mut n) = (0usize, 0usize);
    let mut i = 0;
    while i + 3 < a.bgra.len() {
        n += 1;
        let s = (a.bgra[i] as i32 - b.bgra[i] as i32).abs()
            + (a.bgra[i + 1] as i32 - b.bgra[i + 1] as i32).abs()
            + (a.bgra[i + 2] as i32 - b.bgra[i + 2] as i32).abs();
        if s > 24 {
            d += 1;
        }
        i += 4;
    }
    d as f32 / n.max(1) as f32
}

// ============================================================================
// DWM 缩略图注册
// ============================================================================

unsafe fn register_thumb(host: HWND, src: HWND, w: i32, h: i32) -> Result<isize, String> {
    let mut enabled: BOOL = 0;
    let hr = DwmIsCompositionEnabled(&mut enabled);
    if hr != 0 {
        return Err(format!("DwmIsCompositionEnabled 失败 hr=0x{hr:08X}"));
    }
    if enabled == 0 {
        return Err("DWM 桌面合成未开启（缩略图不可用）".into());
    }
    let mut id: isize = 0;
    let hr = DwmRegisterThumbnail(host, src, &mut id);
    if hr != 0 {
        return Err(format!("DwmRegisterThumbnail 失败 hr=0x{hr:08X}"));
    }
    // 源窗口客户区 1:1 铺满宿主客户区
    let props = DWM_THUMBNAIL_PROPERTIES {
        dwFlags: DWM_TNP_RECTDESTINATION
            | DWM_TNP_RECTSOURCE
            | DWM_TNP_OPACITY
            | DWM_TNP_VISIBLE
            | DWM_TNP_SOURCECLIENTAREAONLY,
        rcDestination: RECT { left: 0, top: 0, right: w, bottom: h },
        rcSource: RECT { left: 0, top: 0, right: w, bottom: h },
        opacity: 255,
        fVisible: 1,
        fSourceClientAreaOnly: 1,
    };
    let hr = DwmUpdateThumbnailProperties(id, &props);
    if hr != 0 {
        DwmUnregisterThumbnail(id);
        return Err(format!("DwmUpdateThumbnailProperties 失败 hr=0x{hr:08X}"));
    }
    Ok(id)
}

// ============================================================================
// 消息泵
// ============================================================================

unsafe fn pump() {
    let mut msg: MSG = std::mem::zeroed();
    while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

/// 边抽消息边等，避免测试期间窗口不重绘
unsafe fn settle(ms: u64) {
    let step = 10u64;
    let mut left = ms;
    while left > 0 {
        pump();
        std::thread::sleep(Duration::from_millis(step.min(left)));
        left = left.saturating_sub(step);
    }
}

unsafe fn freeze_and_set(target: HWND, tick: u32) {
    FROZEN.store(true, Ordering::SeqCst);
    TICK.store(tick, Ordering::SeqCst);
    InvalidateRect(target, null(), 1);
    settle(260);
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ============================================================================
// 自检：源窗口 + 遮挡窗口 + 宿主窗口，全方式对比
// ============================================================================

struct Ev {
    method: &'static str,
    state: &'static str,
    expected: u32,
    res: Result<EvData, String>,
}

struct EvData {
    w: i32,
    h: i32,
    distinct: usize,
    mean: f32,
    min: u8,
    max: u8,
    quad: f32,
    bits: Option<u32>,
    bit_ok: bool,
    png: String,
}

unsafe fn run_self_test(out_dir: &str) -> i32 {
    std::fs::create_dir_all(out_dir).ok();
    let hinst = GetModuleHandleW(null());

    let cls_target = wide("CanLapseSpikeTarget");
    let cls_occluder = wide("CanLapseSpikeOccluder");
    let cls_host = wide("CanLapseSpikeHost");

    let wc = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(wndproc_target),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinst,
        hIcon: null_mut(),
        hCursor: null_mut(),
        hbrBackground: null_mut(),
        lpszMenuName: null(),
        lpszClassName: null(),
    };
    let mut wc_target = wc;
    wc_target.lpszClassName = cls_target.as_ptr();
    let mut wc_occ = wc;
    wc_occ.lpfnWndProc = Some(wndproc_occluder);
    wc_occ.lpszClassName = cls_occluder.as_ptr();
    let mut wc_host = wc;
    wc_host.lpfnWndProc = Some(wndproc_host);
    wc_host.lpszClassName = cls_host.as_ptr();

    if RegisterClassW(&wc_target) == 0
        || RegisterClassW(&wc_occ) == 0
        || RegisterClassW(&wc_host) == 0
    {
        eprintln!("RegisterClassW 失败");
        return 2;
    }

    // 布局：源窗口在左，遮挡窗口盖住它，宿主窗口放到不重叠的地方
    let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
    let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
    let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
    let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
    println!("虚拟桌面: origin=({vx},{vy}) size={vw}x{vh}");

    let (tx, ty) = (vx + 80, vy + 80);
    // 遮挡窗口必须完整盖住源窗口：源窗口 x∈[vx+80, vx+560]，y∈[vy+80, vy+400]
    let (ox, oy) = (vx + 40, vy + 40);
    let (ow, oh) = (TW + 80, TH + 80);
    // 宿主窗口必须与遮挡窗口**完全**不重叠，否则 M7 会被遮挡窗口污染
    let (hx, hy) = if vw >= 660 + TW + 20 {
        (vx + 660, vy + 80)
    } else {
        (vx + 80, vy + oh + 80)
    };

    let target = CreateWindowExW(
        0,
        cls_target.as_ptr(),
        wide("CanLapse 原型·源窗口").as_ptr(),
        WS_POPUP | WS_VISIBLE,
        tx,
        ty,
        TW,
        TH,
        null_mut(),
        null_mut(),
        hinst,
        null(),
    );
    let occluder = CreateWindowExW(
        0,
        cls_occluder.as_ptr(),
        wide("CanLapse 原型·遮挡窗口").as_ptr(),
        WS_POPUP | WS_VISIBLE,
        ox,
        oy,
        ow,
        oh,
        null_mut(),
        null_mut(),
        hinst,
        null(),
    );
    let host = CreateWindowExW(
        0,
        cls_host.as_ptr(),
        wide("CanLapse 原型·缩略图宿主").as_ptr(),
        WS_POPUP | WS_VISIBLE,
        hx,
        hy,
        TW,
        TH,
        null_mut(),
        null_mut(),
        hinst,
        null(),
    );
    if target.is_null() || occluder.is_null() || host.is_null() {
        eprintln!("CreateWindowExW 失败");
        return 2;
    }
    ShowWindow(target, SW_SHOWNORMAL);
    ShowWindow(occluder, SW_SHOWNORMAL);
    ShowWindow(host, SW_SHOWNORMAL);
    UpdateWindow(target);
    UpdateWindow(host);
    SetTimer(target, 1, 120, None);
    settle(700);
    println!("源窗口 hwnd=0x{:X}  遮挡窗口 hwnd=0x{:X}  宿主 hwnd=0x{:X}", target as isize, occluder as isize, host as isize);

    let thumb = register_thumb(host, target, TW, TH);
    match &thumb {
        Ok(id) => println!("DwmRegisterThumbnail 成功 id={id}"),
        Err(e) => println!("DwmRegisterThumbnail 失败: {e}"),
    }
    settle(700);

    // 宿主原始位置，供离屏测试后复位
    let (hx0, hy0) = (hx, hy);

    let states = ["visible", "occluded"];
    let mut evs: Vec<Ev> = Vec::new();

    for (si, state) in states.iter().enumerate() {
        let state: &'static str = state;
        // state 切换：occluded 时把遮挡窗口提到最前，visible 时把源窗口提到最前
        if state == "occluded" {
            SetWindowPos(occluder, HWND_TOPMOST, ox, oy, ow, oh, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            SetForegroundWindow(occluder);
        } else {
            SetWindowPos(target, HWND_TOPMOST, tx, ty, TW, TH, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            SetForegroundWindow(target);
        }
        settle(600);

        // 每个方法用不同的 tick，便于确认抓到的确实是"当时那一帧"
        let base: u32 = if si == 0 { 0x0110 } else { 0x1200 };
        let mut mi: u32 = 0;

        macro_rules! do_capture {
            ($name:expr, $f:expr) => {{
                let expected = base + mi;
                freeze_and_set(target, expected);
                let r: Result<Cap, String> = $f;
                let ev = match r {
                    Ok(cap) => {
                        let st = stats(&cap);
                        let quad = quadrant_score(&cap);
                        let bits = bit_value(&cap);
                        let png = format!("{}/{}_{}.png", out_dir, state, $name);
                        let _ = cap.save_png(&png);
                        EvData {
                            w: cap.w,
                            h: cap.h,
                            distinct: st.distinct,
                            mean: st.mean,
                            min: st.min,
                            max: st.max,
                            quad,
                            bit_ok: bits == Some(expected),
                            bits,
                            png,
                        }
                    }
                    Err(e) => {
                        evs.push(Ev { method: $name, state, expected, res: Err(e) });
                        mi += 1;
                        continue;
                    }
                };
                evs.push(Ev { method: $name, state, expected, res: Ok(ev) });
                mi += 1;
            }};
        }

        do_capture!("M1_screen_bitblt", capture_screen_rect(&client_screen_rect(target)));
        do_capture!("M2_printwindow_fullcontent", capture_printwindow(target, 2));
        do_capture!("M3_printwindow_plain", capture_printwindow(target, 0));

        if thumb.is_ok() {
            do_capture!("M4_host_printwindow_fullcontent", capture_printwindow(host, 2));
            do_capture!("M5_host_bitblt", capture_window_dc(host));
            do_capture!("M7_host_screen_bitblt", capture_screen_rect(&client_screen_rect(host)));

            // M6：宿主移出屏幕后再抓
            SetWindowPos(host, null_mut(), vx - 4000, hy0, TW, TH, SWP_NOACTIVATE);
            settle(500);
            do_capture!("M6_host_offscreen_printwindow", capture_printwindow(host, 2));
            SetWindowPos(host, null_mut(), hx0, hy0, TW, TH, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            settle(400);
        }
    }

    // ---- 报告 ----
    println!("\n================= 结果 =================");
    println!(
        "{:<34} {:<9} {:>6} {:>8} {:>7} {:>5} {:>10} {:>7}",
        "方法", "状态", "非空色数", "均值", "象限得分", "位值", "位匹配", "尺寸"
    );
    for ev in &evs {
        match &ev.res {
            Ok(d) => {
                println!(
                    "{:<34} {:<9} {:>6} {:>8.1} {:>7.2} {:>5} {:>10} {:>7}",
                    ev.method,
                    ev.state,
                    d.distinct,
                    d.mean,
                    d.quad,
                    d.bits.map(|b| b.to_string()).unwrap_or_else(|| "-".into()),
                    if d.bit_ok { "是" } else { "否" },
                    format!("{}x{}", d.w, d.h),
                );
            }
            Err(e) => {
                println!("{:<34} {:<9} 失败: {e}", ev.method, ev.state);
            }
        }
    }

    // 判定：象限得分 >= 0.9 且 16 位计数条完全匹配，才算"真的抓到了源窗口内容"
    let usable = |state: &str| -> Vec<String> {
        evs.iter()
            .filter(|e| e.state == state)
            .filter_map(|e| match &e.res {
                Ok(d) if d.quad >= 0.9 && d.bit_ok => Some(e.method.to_string()),
                _ => None,
            })
            .collect()
    };
    let vis = usable("visible");
    let occ = usable("occluded");

    println!("\n可见时可用（内容自证通过）: {}", if vis.is_empty() { "无".into() } else { vis.join(", ") });
    println!("被遮挡时可用（内容自证通过）: {}", if occ.is_empty() { "无".into() } else { occ.join(", ") });

    // DWM 缩略图是否真的被合成上去了：看 M7（宿主矩形的屏幕像素）
    let m7 = evs.iter().find(|e| e.method == "M7_host_screen_bitblt" && e.state == "visible");
    let m4 = evs.iter().find(|e| e.method == "M4_host_printwindow_fullcontent" && e.state == "visible");
    println!("\n---- 关于 DWM 缩略图的关键判定 ----");
    match m7.and_then(|e| e.res.as_ref().ok()) {
        Some(d) if d.quad >= 0.9 => println!("宿主窗口在屏幕上确实显示出了源窗口画面 => 缩略图合成成功"),
        Some(d) => println!(
            "宿主窗口屏幕像素不像源窗口内容（象限得分 {:.2}，均值 {:.1}）=> 缩略图**没有**合成上来（若出现品红 255,0,255 即宿主自绘底色）",
            d.quad, d.mean
        ),
        None => println!("M7 未取得结果，无法判定"),
    }
    match m4.and_then(|e| e.res.as_ref().ok()) {
        Some(d) if d.quad >= 0.9 && d.bit_ok => {
            println!("★ 结论：通过 \"宿主 + PrintWindow(PW_RENDERFULLCONTENT)\" **可以读到**缩略图像素")
        }
        Some(d) => println!(
            "结论：宿主 + PrintWindow 读到的东西不是源窗口内容（象限得分 {:.2}）=> 缩略图像素**无法**这样读回",
            d.quad
        ),
        None => println!("结论：宿主 + PrintWindow 方式未能取得结果"),
    }

    if let Ok(id) = thumb {
        DwmUnregisterThumbnail(id);
    }
    let _ = std::fs::write(
        format!("{out_dir}/README.txt"),
        "本目录是 dwm-capture-spike 的运行产物：每种读回方式 × 遮挡状态各一张 PNG。\n\
         文件名形如 visible_M4_host_printwindow_fullcontent.png。\n\
         判定要看内容是否与源窗口图案一致，而不是只看是不是黑屏。\n",
    );
    0
}

// ============================================================================
// --list / --hwnd / --title
// ============================================================================

static FOUND: Mutex<Vec<(isize, String, String, u32, String)>> = Mutex::new(Vec::new());

unsafe fn proc_name(pid: u32) -> String {
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

fn text_of_w(hwnd: HWND, f: unsafe extern "system" fn(HWND, *mut u16, i32) -> i32) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { f(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

unsafe extern "system" fn enum_cb(hwnd: HWND, _l: LPARAM) -> BOOL {
    if IsWindowVisible(hwnd) == 0 {
        return 1;
    }
    let title = text_of_w(hwnd, GetWindowTextW);
    let class = text_of_w(hwnd, GetClassNameW);
    if title.is_empty() && class.is_empty() {
        return 1;
    }
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, &mut pid);
    let exe = proc_name(pid);
    FOUND.lock().unwrap().push((hwnd as isize, title, class, pid, exe));
    1
}

unsafe fn cmd_list() -> i32 {
    enumerate_all();
    let v = FOUND.lock().unwrap();
    println!("{:<12} {:<24} {:<24} {:>7}  {}", "HWND", "进程", "类名", "PID", "标题");
    let mut rows = v.clone();
    rows.sort_by_key(|r| r.0);
    for (h, title, class, pid, exe) in rows {
        println!("0x{:<10X} {:<24} {:<24} {:>7}  {}", h, exe, class, pid, title);
    }
    println!("\n共 {} 个可见顶层窗口", v.len());
    0
}

/// 重新枚举一遍所有可见顶层窗口（清空后重填，避免累积重复）
unsafe fn enumerate_all() {
    FOUND.lock().unwrap().clear();
    EnumWindows(Some(enum_cb), 0);
}

unsafe fn find_by_title(sub: &str) -> Option<HWND> {
    enumerate_all();
    let v = FOUND.lock().unwrap();
    let low = sub.to_lowercase();
    v.iter()
        .find(|r| r.1.to_lowercase().contains(&low))
        .map(|r| r.0 as HWND)
}

unsafe fn find_by_class(cls: &str) -> Option<HWND> {
    enumerate_all();
    let v = FOUND.lock().unwrap();
    v.iter().find(|r| r.2 == cls).map(|r| r.0 as HWND)
}

unsafe fn run_on_hwnd(hwnd: HWND, out_dir: &str) -> i32 {
    std::fs::create_dir_all(out_dir).ok();
    if IsWindow(hwnd) == 0 {
        eprintln!("hwnd 0x{:X} 无效", hwnd as isize);
        return 2;
    }
    let title = text_of_w(hwnd, GetWindowTextW);
    let class = text_of_w(hwnd, GetClassNameW);
    let mut wr: RECT = std::mem::zeroed();
    GetWindowRect(hwnd, &mut wr);
    println!(
        "目标 hwnd=0x{:X} 标题=\"{}\" 类名=\"{}\" 窗口矩形={}x{}",
        hwnd as isize,
        title,
        class,
        wr.right - wr.left,
        wr.bottom - wr.top
    );

    // 建一个宿主窗口并注册缩略图
    let hinst = GetModuleHandleW(null());
    let cls_host = wide("CanLapseSpikeHost");
    let wc = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(wndproc_host),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinst,
        hIcon: null_mut(),
        hCursor: null_mut(),
        hbrBackground: null_mut(),
        lpszMenuName: null(),
        lpszClassName: cls_host.as_ptr(),
    };
    RegisterClassW(&wc);
    let mut cr: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut cr);
    let (cw, ch) = (cr.right - cr.left, cr.bottom - cr.top);
    let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
    let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
    let host = CreateWindowExW(
        0,
        cls_host.as_ptr(),
        wide("CanLapse 宿主").as_ptr(),
        WS_POPUP | WS_VISIBLE,
        vx + 20,
        vy + 20,
        cw.min(1280),
        ch.min(800),
        null_mut(),
        null_mut(),
        hinst,
        null(),
    );
    ShowWindow(host, SW_SHOWNORMAL);
    settle(400);
    let thumb = register_thumb(host, hwnd, cw.min(1280), ch.min(800));
    println!("DwmRegisterThumbnail: {:?}", thumb.as_ref().map_err(|e| e.clone()));
    settle(600);

    // 用类型别名，避免 Box<dyn Fn() -> Result<..>> 的尖括号把解析器绕晕
    type CaptureFn = Box<dyn Fn() -> Result<Cap, String>>;
    let m1: CaptureFn = Box::new(move || capture_screen_rect(&client_screen_rect(hwnd)));
    let m2: CaptureFn = Box::new(move || capture_printwindow(hwnd, 2));
    let m3: CaptureFn = Box::new(move || capture_printwindow(hwnd, 0));
    let m4: CaptureFn = Box::new(move || capture_printwindow(host, 2));
    let m5: CaptureFn = Box::new(move || capture_window_dc(host));
    let m7: CaptureFn = Box::new(move || capture_screen_rect(&client_screen_rect(host)));
    let methods: Vec<(&str, CaptureFn)> = vec![
        ("M1_screen_bitblt", m1),
        ("M2_printwindow_fullcontent", m2),
        ("M3_printwindow_plain", m3),
        ("M4_host_printwindow_fullcontent", m4),
        ("M5_host_bitblt", m5),
        ("M7_host_screen_bitblt", m7),
    ];

    println!("\n{:<34} {:>7} {:>8} {:>7} {:>6} {:>5}", "方法", "尺寸", "非空色数", "均值", "最小", "最大");
    for (name, f) in &methods {
        match f() {
            Ok(cap) => {
                let st = stats(&cap);
                let png = format!("{out_dir}/{name}.png");
                let _ = cap.save_png(&png);
                println!(
                    "{:<34} {:>7} {:>8} {:>7.1} {:>6} {:>5}   -> {png}",
                    name,
                    format!("{}x{}", cap.w, cap.h),
                    st.distinct,
                    st.mean,
                    st.min,
                    st.max
                );
            }
            Err(e) => println!("{:<34} 失败: {e}", name),
        }
    }
    if let Ok(id) = thumb {
        DwmUnregisterThumbnail(id);
    }
    0
}

// ============================================================================
// 跨进程 + 遮挡 + 最小化 的严格验证
// ============================================================================

const SW_MINIMIZE_V: i32 = 6; // SW_MINIMIZE
const SW_RESTORE_V: i32 = 9; // SW_RESTORE

/// 按顺序跑全部读回方式，并把每张图存成 PNG
unsafe fn capture_all(
    hwnd: HWND,
    host: HWND,
    out_dir: &str,
    state: &str,
    thumb_ok: bool,
) -> Vec<(&'static str, Result<Cap, String>)> {
    type CaptureFn = Box<dyn Fn() -> Result<Cap, String>>;
    let m1: CaptureFn = Box::new(move || capture_screen_rect(&client_screen_rect(hwnd)));
    let m2: CaptureFn = Box::new(move || capture_printwindow(hwnd, 2));
    let m3: CaptureFn = Box::new(move || capture_printwindow(hwnd, 0));
    let m4: CaptureFn = Box::new(move || capture_printwindow(host, 2));
    let m5: CaptureFn = Box::new(move || capture_window_dc(host));
    let m7: CaptureFn = Box::new(move || capture_screen_rect(&client_screen_rect(host)));

    let mut methods: Vec<(&'static str, CaptureFn)> = vec![
        ("M1_screen_bitblt", m1),
        ("M2_printwindow_fullcontent", m2),
        ("M3_printwindow_plain", m3),
    ];
    if thumb_ok {
        methods.push(("M4_host_printwindow_fullcontent", m4));
        methods.push(("M5_host_bitblt", m5));
        methods.push(("M7_host_screen_bitblt", m7));
    }

    let mut out = Vec::new();
    for (name, f) in &methods {
        let r = f();
        if let Ok(cap) = &r {
            let _ = cap.save_png(&format!("{out_dir}/{state}_{name}.png"));
        }
        out.push((*name, r));
    }
    out
}

unsafe fn make_host(w: i32, h: i32, x: i32, y: i32) -> HWND {
    let hinst = GetModuleHandleW(null());
    let cls: Vec<u16> = "CanLapseSpikeHostX\0".encode_utf16().collect();
    let wc = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(wndproc_host),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinst,
        hIcon: null_mut(),
        hCursor: null_mut(),
        hbrBackground: null_mut(),
        lpszMenuName: null(),
        lpszClassName: cls.as_ptr(),
    };
    RegisterClassW(&wc);
    let h = CreateWindowExW(
        0,
        cls.as_ptr(),
        wide("CanLapse 宿主").as_ptr(),
        WS_POPUP | WS_VISIBLE,
        x,
        y,
        w,
        h,
        null_mut(),
        null_mut(),
        hinst,
        null(),
    );
    ShowWindow(h, SW_SHOWNORMAL);
    h
}

unsafe fn make_cover(x: i32, y: i32, w: i32, h: i32) -> HWND {
    let hinst = GetModuleHandleW(null());
    let cls: Vec<u16> = "CanLapseSpikeCover\0".encode_utf16().collect();
    let wc = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(wndproc_occluder),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinst,
        hIcon: null_mut(),
        hCursor: null_mut(),
        hbrBackground: null_mut(),
        lpszMenuName: null(),
        lpszClassName: cls.as_ptr(),
    };
    RegisterClassW(&wc);
    let hwnd = CreateWindowExW(
        0,
        cls.as_ptr(),
        wide("CanLapse 遮挡").as_ptr(),
        WS_POPUP | WS_VISIBLE,
        x,
        y,
        w,
        h,
        null_mut(),
        null_mut(),
        hinst,
        null(),
    );
    ShowWindow(hwnd, SW_SHOWNORMAL);
    SetWindowPos(
        hwnd,
        HWND_TOPMOST,
        x,
        y,
        w,
        h,
        SWP_NOACTIVATE | SWP_SHOWWINDOW,
    );
    SetForegroundWindow(hwnd);
    hwnd
}

fn parse_tick(title: &str) -> Option<u32> {
    let i = title.find("tick=")? + 5;
    let digits: String = title[i..].chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// 跨进程设定对端 tick 并读回"它自称的 tick"，确认设定生效
unsafe fn set_remote_tick(hwnd: HWND, tick: u32) -> Option<u32> {
    PostMessageW(hwnd, WM_APP_SET_TICK, tick as WPARAM, 0);
    settle(350);
    let t = text_of_w(hwnd, GetWindowTextW);
    parse_tick(&t)
}

fn find_cap<'a>(
    caps: &'a [(&'static str, Result<Cap, String>)],
    name: &str,
) -> Option<&'a Cap> {
    caps.iter()
        .find(|(n, _)| *n == name)
        .and_then(|(_, r)| r.as_ref().ok())
}

/// `--helper`：作为"另一个进程"存在，画出确定性图案并把 tick 写进标题
unsafe fn run_helper() -> i32 {
    let hinst = GetModuleHandleW(null());
    let cls: Vec<u16> = "CanLapseSpikeHelper\0".encode_utf16().collect();
    let wc = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(wndproc_helper),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinst,
        hIcon: null_mut(),
        hCursor: null_mut(),
        hbrBackground: null_mut(),
        lpszMenuName: null(),
        lpszClassName: cls.as_ptr(),
    };
    if RegisterClassW(&wc) == 0 {
        eprintln!("helper: RegisterClassW 失败");
        return 2;
    }
    let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
    let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
    // 允许用 SPIKE_HELPER_SIZE=WxH 指定对端窗口尺寸（用于 4K 等大画布验证）
    let (win_w, win_h) = std::env::var("SPIKE_HELPER_SIZE")
        .ok()
        .and_then(|s| {
            let (a, b) = s.split_once(['x', 'X'])?;
            Some((a.trim().parse::<i32>().ok()?, b.trim().parse::<i32>().ok()?))
        })
        .unwrap_or((TW, TH));
    println!("helper 窗口尺寸: {win_w}x{win_h}");
    let hwnd = CreateWindowExW(
        0,
        cls.as_ptr(),
        wide("CanLapseSpikeHelper tick=0").as_ptr(),
        WS_POPUP | WS_VISIBLE,
        vx + 200,
        vy + 200,
        win_w,
        win_h,
        null_mut(),
        null_mut(),
        hinst,
        null(),
    );
    if hwnd.is_null() {
        eprintln!("helper: CreateWindowExW 失败");
        return 2;
    }
    ShowWindow(hwnd, SW_SHOWNORMAL);
    SetTimer(hwnd, 1, 150, None);
    println!("helper 就绪 hwnd=0x{:X} pid={}", hwnd as isize, std::process::id());
    while !HELPER_QUIT.load(Ordering::SeqCst) {
        pump();
        std::thread::sleep(Duration::from_millis(30));
    }
    0
}

/// `--cross`：起一个独立进程作为目标窗口，在 正常/被遮挡/已最小化 三态下严格自证
unsafe fn run_cross(out_dir: &str) -> i32 {
    std::fs::create_dir_all(out_dir).ok();
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("拿不到自身路径: {e}");
            return 2;
        }
    };
    let mut child = match std::process::Command::new(&exe).arg("--helper").spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("启动 helper 子进程失败: {e}");
            return 2;
        }
    };
    println!("本进程 pid={}，已启动对端 helper pid={}", std::process::id(), child.id());

    let mut helper: Option<HWND> = None;
    for _ in 0..120 {
        pump();
        if let Some(h) = find_by_class("CanLapseSpikeHelper") {
            helper = Some(h);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let helper = match helper {
        Some(h) => h,
        None => {
            eprintln!("没等到对端窗口出现");
            let _ = child.kill();
            return 2;
        }
    };
    let mut rpid: u32 = 0;
    GetWindowThreadProcessId(helper, &mut rpid);
    println!(
        "对端窗口 hwnd=0x{:X} 属于 pid={}（跨进程: {}）",
        helper as isize,
        rpid,
        rpid != std::process::id()
    );
    settle(500);

    let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
    let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
    // 宿主尺寸必须等于**源窗口客户区**尺寸，才能保证缩略图 1:1 不缩放
    let mut cr: RECT = std::mem::zeroed();
    GetClientRect(helper, &mut cr);
    let (sw, sh) = (cr.right - cr.left, cr.bottom - cr.top);
    println!("对端客户区尺寸: {sw}x{sh}（宿主按此尺寸建立以保证 1:1）");
    let host = make_host(sw, sh, vx + 900, vy + 200);
    settle(300);
    let thumb = register_thumb(host, helper, sw, sh);
    println!(
        "DwmRegisterThumbnail(本进程宿主 -> 跨进程源): {:?}",
        thumb.as_ref().map_err(|e| e.clone())
    );
    settle(500);

    let mut hw: RECT = std::mem::zeroed();
    GetWindowRect(helper, &mut hw);
    let (hx, hy) = (hw.left, hw.top);
    let (hw_w, hw_h) = (hw.right - hw.left, hw.bottom - hw.top);
    let cover = make_cover(hx - 20, hy - 20, hw_w + 40, hw_h + 40);

    let thumb_ok = thumb.is_ok();
    let mut passes: Vec<(&'static str, u32, Vec<(&'static str, Result<Cap, String>)>)> = Vec::new();

    // ---- 状态 1：正常（对端可见并置顶）----
    SetWindowPos(helper, HWND_TOPMOST, hx, hy, hw_w, hw_h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
    SetForegroundWindow(helper);
    settle(600);
    let t1 = set_remote_tick(helper, 0x0201);
    println!("状态 normal: 对端自称 tick={:?}", t1);
    passes.push((
        "normal",
        t1.unwrap_or(0x0201),
        capture_all(helper, host, out_dir, "normal", thumb_ok),
    ));

    // ---- 状态 2：被本进程的遮挡窗口完整盖住 ----
    SetWindowPos(cover, HWND_TOPMOST, hx - 20, hy - 20, hw_w + 40, hw_h + 40, SWP_NOACTIVATE | SWP_SHOWWINDOW);
    SetForegroundWindow(cover);
    settle(700);
    let t2 = set_remote_tick(helper, 0x0302);
    println!("状态 occluded: 对端自称 tick={:?}", t2);
    passes.push((
        "occluded",
        t2.unwrap_or(0x0302),
        capture_all(helper, host, out_dir, "occluded", thumb_ok),
    ));

    // ---- 状态 3：对端被最小化 ----
    // 注意：最小化后窗口不会重绘，所以"画面里的 tick"应当仍是上一次已绘制的那一帧，
    // 因此这里不改变 tick，预期值沿用 t2。
    ShowWindow(helper, SW_MINIMIZE_V);
    settle(800);
    println!("状态 minimized: IsIconic={}", IsIconic(helper));
    passes.push((
        "minimized",
        t2.unwrap_or(0x0302),
        capture_all(helper, host, out_dir, "minimized", thumb_ok),
    ));
    ShowWindow(helper, SW_RESTORE_V);
    settle(400);

    // ---- 报告 ----
    println!("\n================= 跨进程结果 =================");
    println!(
        "{:<34} {:<10} {:>7} {:>8} {:>9} {:>6} {:>7} {:>6}",
        "方法", "状态", "预期tick", "读到位值", "象限得分", "均值", "位匹配", "角标"
    );
    for (state, exp, caps) in &passes {
        for (name, r) in caps {
            match r {
                Ok(cap) => {
                    let st = stats(cap);
                    let quad = quadrant_score(cap);
                    let bits = bit_value(cap);
                    println!(
                        "{:<34} {:<10} {:>7} {:>8} {:>9.2} {:>6.1} {:>7} {:>6}",
                        name,
                        state,
                        exp,
                        bits.map(|b| b.to_string()).unwrap_or_else(|| "-".into()),
                        quad,
                        st.mean,
                        if bits == Some(*exp) { "是" } else { "否" },
                        if corner_marker_ok(cap, cap.w, cap.h) { "是" } else { "否" }
                    );
                }
                Err(e) => println!("{:<34} {:<10} 失败: {e}", name, state),
            }
        }
    }

    // ---- 遮挡/最小化前后的差异矩阵 ----
    println!("\n---- 差异矩阵（不同像素占比，0 = 完全一致）----");
    let names: Vec<&str> = passes[0].2.iter().map(|(n, _)| *n).collect();
    println!("{:<34} {:>14} {:>14}", "方法", "正常 vs 遮挡", "正常 vs 最小化");
    for n in &names {
        let a = find_cap(&passes[0].2, n);
        let b = find_cap(&passes[1].2, n);
        let c = find_cap(&passes[2].2, n);
        let d1 = match (a, b) {
            (Some(x), Some(y)) => format!("{:.3}", diff_ratio(x, y)),
            _ => "-".into(),
        };
        let d2 = match (a, c) {
            (Some(x), Some(y)) => format!("{:.3}", diff_ratio(x, y)),
            _ => "-".into(),
        };
        println!("{:<34} {:>14} {:>14}", n, d1, d2);
    }

    // ---- 结论 ----
    println!("\n---- 结论（跨进程，内容经位条严格自证）----");
    let verdict = |state: &str, name: &str| -> &'static str {
        let p = passes.iter().find(|(s, _, _)| *s == state);
        match p {
            Some((_, exp, caps)) => match find_cap(caps, name) {
                Some(cap) => {
                    if quadrant_score(cap) >= 0.9
                        && bit_value(cap) == Some(*exp)
                        && corner_marker_ok(cap, cap.w, cap.h)
                    {
                        "可用"
                    } else {
                        "不可用"
                    }
                }
                None => "未取到",
            },
            None => "未测",
        }
    };
    for name in &names {
        println!(
            "{:<34} 正常={:<6} 遮挡={:<6} 最小化={:<6}",
            name,
            verdict("normal", name),
            verdict("occluded", name),
            verdict("minimized", name)
        );
    }

    if let Ok(id) = thumb {
        DwmUnregisterThumbnail(id);
    }
    DestroyWindow(cover);
    DestroyWindow(host);
    let _ = child.kill();
    let _ = child.wait();
    0
}

/// `--foreign`：对任意真实窗口跑 正常/被遮挡/已最小化 三态，输出差异矩阵。
/// 真实 App 没有我们的位条，无法像对端窗口那样自证，所以用"三态互相比对"来判定：
///   * M1 屏幕抓取：被遮挡后差异应当接近 1.0（看到的是遮挡窗口）——这是对照
///   * M4 缩略图宿主：被遮挡/最小化后差异应当接近 0（仍在拿到目标窗口内容）
unsafe fn run_foreign(hwnd: HWND, out_dir: &str) -> i32 {
    std::fs::create_dir_all(out_dir).ok();
    if IsWindow(hwnd) == 0 {
        eprintln!("hwnd 0x{:X} 无效", hwnd as isize);
        return 2;
    }
    let title = text_of_w(hwnd, GetWindowTextW);
    let class = text_of_w(hwnd, GetClassNameW);
    let mut rpid: u32 = 0;
    GetWindowThreadProcessId(hwnd, &mut rpid);
    let mut hw: RECT = std::mem::zeroed();
    GetWindowRect(hwnd, &mut hw);
    let mut cr: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut cr);
    println!(
        "目标 hwnd=0x{:X} 标题=\"{}\" 类名=\"{}\" 进程={} 窗口={}x{} 客户区={}x{}",
        hwnd as isize,
        title,
        class,
        proc_name(rpid),
        hw.right - hw.left,
        hw.bottom - hw.top,
        cr.right,
        cr.bottom
    );

    let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
    let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
    let (hx, hy) = (hw.left, hw.top);
    let (hw_w, hw_h) = (hw.right - hw.left, hw.bottom - hw.top);
    let host = make_host(hw_w.min(1280), hw_h.min(800), vx + 40, vy + 40);
    settle(300);
    let thumb = register_thumb(host, hwnd, hw_w.min(1280), hw_h.min(800));
    println!(
        "DwmRegisterThumbnail: {:?}",
        thumb.as_ref().map_err(|e| e.clone())
    );
    settle(500);
    let cover = make_cover(hx - 20, hy - 20, hw_w + 40, hw_h + 40);
    let thumb_ok = thumb.is_ok();

    let mut passes: Vec<(&'static str, Option<bool>, Vec<(&'static str, Result<Cap, String>)>)> =
        Vec::new();

    // 状态 1：正常（目标置顶可见）
    SetWindowPos(hwnd, HWND_TOPMOST, hx, hy, hw_w, hw_h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
    SetForegroundWindow(hwnd);
    settle(800);
    passes.push((
        "normal",
        None,
        capture_all(hwnd, host, out_dir, "normal", thumb_ok),
    ));

    // 状态 2：被本进程遮挡窗口完整盖住
    SetWindowPos(cover, HWND_TOPMOST, hx - 20, hy - 20, hw_w + 40, hw_h + 40, SWP_NOACTIVATE | SWP_SHOWWINDOW);
    SetForegroundWindow(cover);
    settle(900);
    passes.push((
        "occluded",
        None,
        capture_all(hwnd, host, out_dir, "occluded", thumb_ok),
    ));

    // 状态 3：目标被最小化
    ShowWindow(hwnd, SW_MINIMIZE_V);
    settle(900);
    let iconic = IsIconic(hwnd) != 0;
    println!("目标已最小化: IsIconic={}", if iconic { 1 } else { 0 });
    passes.push((
        "minimized",
        Some(iconic),
        capture_all(hwnd, host, out_dir, "minimized", thumb_ok),
    ));
    ShowWindow(hwnd, SW_RESTORE_V);
    settle(500);

    // ---- 报告：三态差异矩阵 ----
    let names: Vec<&str> = passes[0].2.iter().map(|(n, _)| *n).collect();
    println!("\n---- 差异矩阵（不同像素占比；越小越说明抓到的是同一内容）----");
    println!(
        "{:<34} {:>16} {:>16} {:>16}",
        "方法", "正常 vs 遮挡", "正常 vs 最小化", "遮挡 vs 最小化"
    );
    for n in &names {
        let a = find_cap(&passes[0].2, n);
        let b = find_cap(&passes[1].2, n);
        let c = find_cap(&passes[2].2, n);
        let f = |x: Option<&Cap>, y: Option<&Cap>| -> String {
            match (x, y) {
                (Some(p), Some(q)) => format!("{:.3}", diff_ratio(p, q)),
                _ => "-".into(),
            }
        };
        println!(
            "{:<34} {:>16} {:>16} {:>16}",
            n,
            f(a, b),
            f(a, c),
            f(b, c)
        );
    }

    println!("\n---- 判读要点 ----");
    println!("M1 正常 vs 遮挡 应接近 1.000：证明遮挡真的发生了（看到的是遮挡窗口）");
    println!("M4 正常 vs 遮挡 接近 0    ：证明缩略图在目标被完全遮挡时仍在提供内容");
    println!("M4 正常 vs 最小化 接近 0  ：证明目标最小化后仍能取到画面（对录屏产品是关键能力）");

    if let Ok(id) = thumb {
        DwmUnregisterThumbnail(id);
    }
    DestroyWindow(cover);
    DestroyWindow(host);
    0
}

// ============================================================================

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut out_dir = "out".to_string();
    let mut i = 1;
    let mut mode_hwnd: Option<isize> = None;
    let mut mode_title: Option<String> = None;
    let mut mode_list = false;
    let mut mode_helper = false;
    let mut mode_cross = false;
    let mut mode_foreign = false;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                if i < args.len() {
                    out_dir = args[i].clone();
                }
            }
            "--hwnd" => {
                i += 1;
                if i < args.len() {
                    let s = args[i].trim_start_matches("0x").trim_start_matches("0X");
                    mode_hwnd = isize::from_str_radix(s, 16).ok();
                }
            }
            "--title" => {
                i += 1;
                if i < args.len() {
                    mode_title = Some(args[i].clone());
                }
            }
            "--list" => mode_list = true,
            "--helper" => mode_helper = true,
            "--cross" => mode_cross = true,
            "--foreign" => mode_foreign = true,
            _ => {}
        }
        i += 1;
    }

    let code = unsafe {
        if mode_helper {
            run_helper()
        } else if mode_cross {
            run_cross(&out_dir)
        } else if mode_list {
            cmd_list()
        } else if mode_foreign {
            let target = match (mode_hwnd, &mode_title) {
                (Some(h), _) => Some(h as HWND),
                (None, Some(t)) => find_by_title(t),
                _ => None,
            };
            match target {
                Some(h) => run_foreign(h, &out_dir),
                None => {
                    eprintln!("--foreign 需要一个目标：用 --hwnd 0x... 或 --title 子串；--list 可查看");
                    2
                }
            }
        } else if let Some(h) = mode_hwnd {
            run_on_hwnd(h as HWND, &out_dir)
        } else if let Some(t) = mode_title {
            match find_by_title(&t) {
                Some(h) => run_on_hwnd(h, &out_dir),
                None => {
                    eprintln!("按标题 \"{t}\" 没找到窗口；用 --list 看看有哪些");
                    2
                }
            }
        } else {
            run_self_test(&out_dir)
        }
    };
    std::process::exit(code);
}
