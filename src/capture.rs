//! 抓帧核心：**DWM 缩略图宿主窗口 + `PrintWindow(PW_RENDERFULLCONTENT)`**。
//!
//! 为什么是这个组合、以及为什么其他做法都不行，见
//! [ADR 0002](../../docs/decisions/0002-rust-and-dwm-thumbnail-capture.md) 与
//! [spikes/dwm-capture/FINDINGS.md](../../spikes/dwm-capture/FINDINGS.md)。要点：
//!
//! * `DwmRegisterThumbnail` 只把源窗口的缩略图**合成**到宿主上，Win32 没有"取缩略图像素"的 API；
//! * 对**宿主**调 `PrintWindow(..., PW_RENDERFULLCONTENT)` 才能读到合成后的像素（实测）；
//! * `BitBlt` 宿主 DC 读到的永远是宿主自绘内容，读不到缩略图（实测恒为底色）；
//! * 宿主**不需要显示在屏幕上**（实测移到屏幕外 4000px 仍有效），
//!   但仍必须是**真实顶层窗口**（消息窗口不行）。
//!
//! ⚠️ 线程亲和性：GDI 的 DC/位图**只能在创建它的线程上使用**。
//! 因此一个 `ThumbnailCapture` 必须在同一个线程里建、抓、销毁
//! （旧项目曾因此出现"第一次录制成功、之后每次都抓失败"的诡异问题）。

use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::Once;

use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DwmIsCompositionEnabled, DwmRegisterThumbnail, DwmUnregisterThumbnail,
    DwmUpdateThumbnailProperties, DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY,
    DWM_TNP_RECTDESTINATION, DWM_TNP_RECTSOURCE, DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection, CreateSolidBrush, DeleteDC,
    DeleteObject, EndPaint, FillRect, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HDC, HGDIOBJ, PAINTSTRUCT, RGBQUAD, SRCCOPY,
};
use windows_sys::Win32::Storage::Xps::PrintWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetSystemMetrics, ShowWindow,
    SM_XVIRTUALSCREEN, SW_SHOWNOACTIVATE, WM_DESTROY, WM_ERASEBKGND, WM_PAINT, WNDCLASSW, WS_POPUP,
};

use crate::frame::Frame;
use crate::logline;
use crate::win;

const HOST_CLASS: &str = "CanLapseThumbnailHost";
const PW_RENDERFULLCONTENT_FLAG: u32 = 0x2;
static REGISTER_HOST_CLASS: Once = Once::new();

/// `WS_EX_TOOLWINDOW`：宿主窗口**不进任务栏、不进 Alt+Tab**。
/// 少了它，用户把鼠标停在任务栏图标上会看到一堆"CanLapse 抓帧宿主"的预览
/// （实测：每抓一次就多一个，因为每个 ThumbnailCapture 都建一个宿主）。
const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
/// `WS_EX_NOACTIVATE`：宿主永远不抢焦点（它是幕后窗口，不该打断绘画）。
const WS_EX_NOACTIVATE: u32 = 0x0800_0000;

/// 桌面合成是否开启。关闭时 DWM 缩略图不可用，必须提前告诉用户而不是静默坏掉。
pub fn composition_enabled() -> bool {
    let mut enabled: BOOL = 0;
    unsafe { DwmIsCompositionEnabled(&mut enabled) };
    enabled != 0
}

/// 按最大宽度等比缩放输出尺寸（**不放大**）。
///
/// 预览只要 160×100，没必要每帧读整张画布；DWM 在合成阶段就能缩放，比先读大图再缩更省。
fn fit_size(crop: (i32, i32, i32, i32), max_w: i32) -> (i32, i32) {
    let (w, h) = (crop.2, crop.3);
    if max_w <= 0 || max_w == i32::MAX || w <= max_w {
        return (w, h);
    }
    let scale = max_w as f64 / w as f64;
    let nw = max_w.max(1);
    let nh = ((h as f64 * scale).round() as i32).clamp(1, h);
    (nw, nh)
}

unsafe extern "system" fn host_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    win::guard(|| match msg {
        WM_PAINT => {
            // 宿主底色画黑。正常情况下缩略图会把它完全盖住；
            // 一旦 ThumbnailCapture 静默失效，抓到的就是纯黑 —— 由 Frame::is_blank() 告警，
            // 这比"看起来像正常画面"要安全得多。
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut rc);
            let brush = CreateSolidBrush(0x0000_0000);
            FillRect(hdc, &rc, brush);
            DeleteObject(brush as HGDIOBJ);
            EndPaint(hwnd, &ps);
            0
        }
        WM_ERASEBKGND => 1,
        WM_DESTROY => 0,
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    })
}

unsafe fn ensure_host_class() {
    REGISTER_HOST_CLASS.call_once(|| {
        let cls = win::wide(HOST_CLASS);
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(host_wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: win::module_handle(),
            hIcon: null_mut(),
            hCursor: null_mut(),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: cls.as_ptr(),
        };
        unsafe { windows_sys::Win32::UI::WindowsAndMessaging::RegisterClassW(&wc) };
        // cls 在 RegisterClassW 返回后即可释放：Windows 会把类名复制进原子表
    });
}

/// 一块可复用的 32bpp top-down DIB。
///
/// 复用而不是每帧新建：8fps 连续录几小时的话，每帧都 CreateDIBSection/DeleteObject
/// 会白白制造大量 GDI 对象。
struct Dib {
    dc: HDC,
    bmp: *mut c_void,
    old: HGDIOBJ,
    bits: *mut c_void,
    width: i32,
    height: i32,
}

impl Dib {
    fn new(width: i32, height: i32) -> Result<Dib, String> {
        validate_size(width, height)?;
        unsafe {
            let screen = windows_sys::Win32::Graphics::Gdi::GetDC(null_mut());
            if screen.is_null() {
                return Err("GetDC(NULL) 失败".into());
            }
            let dc = CreateCompatibleDC(screen);
            windows_sys::Win32::Graphics::Gdi::ReleaseDC(null_mut(), screen);
            if dc.is_null() {
                return Err("CreateCompatibleDC 失败".into());
            }
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height, // 负数 = top-down，内存行序与直觉一致
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
            let mut bits: *mut c_void = null_mut();
            let bmp = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bmp.is_null() {
                DeleteDC(dc);
                return Err("CreateDIBSection 失败".into());
            }
            if bits.is_null() {
                DeleteObject(bmp as HGDIOBJ);
                DeleteDC(dc);
                return Err("CreateDIBSection 返回空像素指针".into());
            }
            let old = SelectObject(dc, bmp as HGDIOBJ);
            if old.is_null() {
                DeleteObject(bmp as HGDIOBJ);
                DeleteDC(dc);
                return Err("SelectObject(DIB) 失败".into());
            }
            Ok(Dib {
                dc,
                bmp,
                old,
                bits,
                width,
                height,
            })
        }
    }

    fn pixels(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                self.bits as *const u8,
                (self.width as usize) * (self.height as usize) * 4,
            )
        }
    }
}

fn validate_size(width: i32, height: i32) -> Result<(), String> {
    if width <= 0
        || height <= 0
        || width > crate::MAX_CAPTURE_DIM
        || height > crate::MAX_CAPTURE_DIM
    {
        return Err(format!("画面尺寸超出限制：{width}x{height}"));
    }
    let pixels = (width as u64).saturating_mul(height as u64);
    if pixels > crate::MAX_CAPTURE_PIXELS {
        return Err(format!(
            "画面尺寸过大：{width}x{height}（{pixels} 像素，上限 {}）",
            crate::MAX_CAPTURE_PIXELS
        ));
    }
    Ok(())
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bmp as HGDIOBJ);
            DeleteDC(self.dc);
        }
    }
}

/// 一个窗口（或它的某个**子窗口**区域）的持续抓帧器。
///
/// ⚠️ 关键事实（在真实 SAI2 上实测得出）：**`DwmRegisterThumbnail` 不接受子窗口** ——
/// 对子窗口调用返回 `E_INVALIDARG (0x80070057)`，因为 DWM 只合成顶层窗口，
/// 子窗口是画进父窗口表面里的（直接对子窗口 `PrintWindow` 同样失败）。
///
/// 所以抓"画布子窗口"的正确做法是三步：
///   1. 对**顶层窗口**注册缩略图；
///   2. 把 `rcSource` 设成画布子窗口在**顶层客户区坐标系**里的矩形 ——
///      DWM 会把这一块 1:1 送到宿主窗口，不需要我们自己拷整窗再裁剪
///      （SAI2 实测：每帧拷贝量从 1920×986 降到 651×761，约 1/5）；
///   3. 每帧重新查询子窗口矩形 —— 用户拖动画布/改变布局时裁剪框**自动跟随**。
pub struct ThumbnailCapture {
    /// 缩略图源：**顶层窗口**（DWM 只认它）
    root: HWND,
    /// 用户拾取的目标：可能是 root 自己，也可能是它的子窗口
    target: HWND,
    host: HWND,
    thumb: isize,
    dib: Dib,
    /// 裁剪矩形 `(x, y, w, h)`，相对 root 的客户区原点
    crop: (i32, i32, i32, i32),
    /// 输出尺寸（宿主客户区大小）。等于 crop 尺寸时是 1:1；
    /// 小于它时由 DWM 缩放 —— 预览用小图能省掉每帧几 MB 的拷贝（见 [`Self::new_scaled`]）。
    out: (i32, i32),
    /// 缩放上限（宽度）；`i32::MAX` 表示不缩放（录制用，必须 1:1）
    max_w: i32,
}

impl ThumbnailCapture {
    /// 建宿主窗口并注册缩略图。
    ///
    /// `target` 可以是顶层窗口，也可以是它的子窗口（SAI2 的画布就是子窗口）；
    /// 子窗口会自动解析成「顶层窗口 + 裁剪矩形」。
    ///
    /// 宿主放在虚拟桌面之外（屏幕外），用户完全看不到它 —— 实测可行（FINDINGS 实验 1 的 M6）。
    pub fn new(target: HWND) -> Result<Self, String> {
        Self::new_scaled(target, i32::MAX)
    }

    /// 建一个**缩放输出**的抓帧器：DWM 直接把裁剪区域缩到 `max_w` 以内再合成，
    /// 于是 `PrintWindow` 只读一张小图。
    ///
    /// 用途：界面里 160×100 的实时预览，没必要每帧拷整张 1920×986（约 7.5 MB）。
    /// 录制必须用 [`Self::new`]（1:1，像素级差分不能用缩放图）。
    pub fn new_scaled(target: HWND, max_w: i32) -> Result<Self, String> {
        if !win::is_window_alive(target) {
            return Err("目标窗口无效（可能已关闭）".into());
        }
        if !composition_enabled() {
            return Err("桌面合成（DWM）未开启，无法使用缩略图抓帧".into());
        }
        let root = win::root_of(target);
        let crop = win::crop_rect_in_root(target, root)
            .ok_or_else(|| "无法计算目标窗口在顶层窗口中的位置".to_string())?;
        if crop.2 <= 0
            || crop.3 <= 0
            || crop.2 > crate::MAX_CAPTURE_DIM
            || crop.3 > crate::MAX_CAPTURE_DIM
        {
            return Err(format!("目标窗口区域尺寸异常（{}x{}）", crop.2, crop.3));
        }
        let out = fit_size(crop, max_w);
        validate_size(out.0, out.1)?;
        unsafe {
            // 先建 DIB 再建宿主：如果 DIB 失败，不会留下已注册的 DWM 缩略图和宿主窗口。
            let dib = Dib::new(out.0, out.1)?;
            let (host, thumb) = Self::create_host_and_register(root, crop, out)?;
            let me = ThumbnailCapture {
                root,
                target,
                host,
                thumb,
                dib,
                crop,
                out,
                max_w,
            };
            me.apply_properties()?;
            Ok(me)
        }
    }

    unsafe fn create_host_and_register(
        root: HWND,
        crop: (i32, i32, i32, i32),
        out: (i32, i32),
    ) -> Result<(HWND, isize), String> {
        ensure_host_class();
        let cls = win::wide(HOST_CLASS);
        let title = win::wide("CanLapse 抓帧宿主");
        // 放到虚拟桌面左外侧，确保用户看不到
        let off_x = GetSystemMetrics(SM_XVIRTUALSCREEN) - 4000;
        let host = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            cls.as_ptr(),
            title.as_ptr(),
            WS_POPUP, // 不加 WS_VISIBLE，建完再 ShowWindow(SW_SHOWNOACTIVATE)
            off_x,
            0,
            out.0,
            out.1,
            null_mut(),
            null_mut(),
            win::module_handle(),
            null(),
        );
        if host.is_null() {
            logline!("[capture] 创建抓帧宿主失败");
            return Err("创建抓帧宿主窗口失败".into());
        }
        ShowWindow(host, SW_SHOWNOACTIVATE);
        logline!(
            "[capture] 建宿主 0x{:X} {}x{} @x={}  (toolwindow)",
            host as isize,
            out.0,
            out.1,
            off_x
        );

        let mut thumb: isize = 0;
        // ★ 源**必须**是顶层窗口：传子窗口会得到 E_INVALIDARG（SAI2 上实测）
        let hr = DwmRegisterThumbnail(host, root, &mut thumb);
        if hr != 0 {
            DestroyWindow(host);
            logline!(
                "[capture] DwmRegisterThumbnail 失败 hr=0x{hr:08X} root=0x{:X}",
                root as isize
            );
            return Err(format!(
                "DwmRegisterThumbnail 失败 hr=0x{hr:08X}（顶层窗口可能已失效）"
            ));
        }
        logline!(
            "[capture] 注册缩略图 ok thumb={} root=0x{:X} crop=({},{},{},{})",
            thumb,
            root as isize,
            crop.0,
            crop.1,
            crop.2,
            crop.3
        );
        Ok((host, thumb))
    }

    unsafe fn apply_properties(&self) -> Result<(), String> {
        let (cx, cy, cw, ch) = self.crop;
        let (ow, oh) = self.out;
        let props = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION
                | DWM_TNP_RECTSOURCE
                | DWM_TNP_OPACITY
                | DWM_TNP_VISIBLE
                | DWM_TNP_SOURCECLIENTAREAONLY,
            // 目标矩形 = 宿主客户区（可能小于源矩形 => DWM 负责缩放）；
            // 源矩形 = 目标窗口在**源客户区坐标系**里的位置。
            rcDestination: RECT {
                left: 0,
                top: 0,
                right: ow,
                bottom: oh,
            },
            rcSource: RECT {
                left: cx,
                top: cy,
                right: cx + cw,
                bottom: cy + ch,
            },
            opacity: 255,
            fVisible: 1,
            fSourceClientAreaOnly: 1,
        };
        let hr = DwmUpdateThumbnailProperties(self.thumb, &props);
        if hr != 0 {
            return Err(format!("DwmUpdateThumbnailProperties 失败 hr=0x{hr:08X}"));
        }
        Ok(())
    }

    /// 临时关掉缩略图合成（`fVisible=0`）。
    ///
    /// 主窗口最小化、又没有在录制时，DWM 没必要继续为这个宿主合成画面 ——
    /// 关掉能实打实降低后台 GPU/CPU 占用。`PrintWindow` 前必须重新打开。
    pub fn set_visible(&self, visible: bool) -> Result<(), String> {
        let mut props: DWM_THUMBNAIL_PROPERTIES = unsafe { std::mem::zeroed() };
        props.dwFlags = DWM_TNP_VISIBLE;
        props.fVisible = if visible { 1 } else { 0 };
        let hr = unsafe { DwmUpdateThumbnailProperties(self.thumb, &props) };
        if hr != 0 {
            return Err(format!(
                "DwmUpdateThumbnailProperties(visible) 失败 hr=0x{hr:08X}"
            ));
        }
        Ok(())
    }

    pub fn target(&self) -> HWND {
        self.target
    }

    pub fn root(&self) -> HWND {
        self.root
    }

    /// 兼容旧调用名
    pub fn source(&self) -> HWND {
        self.target
    }

    /// 输出尺寸（宿主位图大小）。1:1 抓帧时等于裁剪尺寸；预览缩放时更小。
    pub fn size(&self) -> (i32, i32) {
        self.out
    }

    pub fn source_alive(&self) -> bool {
        win::is_window_alive(self.root) && win::is_window_alive(self.target)
    }

    /// 重新查询目标矩形并让缩略图跟上（子窗口被移动/缩放时这一步是必要的）。
    ///
    /// 返回 `true` 表示**尺寸**变了 —— 调用方应把"上一帧"作废，
    /// 避免拿不同尺寸的两帧去比差分。
    pub fn sync(&mut self) -> Result<bool, String> {
        if !self.source_alive() {
            return Err("目标窗口已关闭".into());
        }
        let crop = win::crop_rect_in_root(self.target, self.root)
            .ok_or_else(|| "无法计算目标窗口位置".to_string())?;
        if crop.2 <= 0
            || crop.3 <= 0
            || crop.2 > crate::MAX_CAPTURE_DIM
            || crop.3 > crate::MAX_CAPTURE_DIM
        {
            return Err(format!("目标窗口区域尺寸异常（{}x{}）", crop.2, crop.3));
        }
        let size_changed = crop.2 != self.crop.2 || crop.3 != self.crop.3;
        let out = fit_size(crop, self.max_w);
        let out_changed = out != self.out;
        let crop_changed = crop != self.crop;
        if size_changed || out_changed {
            unsafe {
                let new_dib = Dib::new(out.0, out.1)?;
                win::pump();
                if self.thumb != 0 {
                    DwmUnregisterThumbnail(self.thumb);
                    self.thumb = 0;
                }
                if !self.host.is_null() {
                    DestroyWindow(self.host);
                    self.host = null_mut();
                }
                let (host, thumb) = Self::create_host_and_register(self.root, crop, out)?;
                self.host = host;
                self.thumb = thumb;
                self.out = out;
                self.dib = new_dib;
            }
        }
        self.crop = crop;
        if crop_changed || size_changed || out_changed {
            unsafe { self.apply_properties()? };
        }
        Ok(size_changed)
    }

    /// 兼容旧调用名
    pub fn sync_source_size(&mut self) -> Result<bool, String> {
        self.sync()
    }

    /// 目标区域被"**同属主窗口**"遮住的比例（见 [`crate::win::covered_ratio`] 的说明）。
    ///
    /// ⚠️ 按用户要求，**当前不用于提醒**：绘画软件自己的浮动面板压在画布上是正常现象，
    /// 照原样录进画面即可（不裁切、不提示）。这个函数保留下来是因为
    /// "画布被遮住多少"仍是可复现的事实，将来若要做"自动裁掉被遮区域"会用到。
    #[allow(dead_code)]
    pub fn covered_ratio(&self) -> f32 {
        win::covered_ratio(self.root, self.crop)
    }

    /// 抓一帧。返回的像素是**源窗口客户区**的内容（1:1 抓帧时；预览缩放时是缩放后的）。
    ///
    /// 目标窗口被其他窗口盖住、甚至被最小化时，本方法依然能取到画面（实测）。
    pub fn grab(&self) -> Result<Frame, String> {
        let (w, h) = self.out;
        let mut f = Frame::new(
            w,
            h,
            vec![0u8; (w as usize).saturating_mul(h as usize).saturating_mul(4)],
        );
        self.grab_into(&mut f)?;
        Ok(f)
    }

    /// 抓一帧到**已有缓冲区**里（录制循环用，避免每帧分配几 MB）。
    ///
    /// 尺寸变了会自动重建缓冲区；调用方拿到的 `frame` 始终是当前尺寸。
    pub fn grab_into(&self, frame: &mut Frame) -> Result<(), String> {
        if !self.source_alive() {
            return Err("目标窗口已关闭".into());
        }
        // 让宿主窗口处理掉待处理消息，保持它"活着"
        win::pump();

        let ok = unsafe { PrintWindow(self.host, self.dib.dc, PW_RENDERFULLCONTENT_FLAG) };
        if ok == 0 {
            return Err("PrintWindow(宿主, PW_RENDERFULLCONTENT) 返回失败".into());
        }
        let (w, h) = self.out;
        let pixels = self.dib.pixels();
        if frame.width != w || frame.height != h || frame.bgra.len() != pixels.len() {
            *frame = Frame::new(w, h, pixels.to_vec());
        } else {
            frame.bgra.copy_from_slice(pixels);
        }
        Ok(())
    }

    /// 只把**发生变化**的 DIB 复制到 `frame`。
    ///
    /// 静态画布下 `prev` 与当前 DIB 相同，直接返回 false，不产生几十 MB 的
    /// DIB→Frame 拷贝；只有确实需要保存时才复制。
    pub fn grab_if_changed(
        &self,
        prev: &Frame,
        threshold: f32,
        frame: &mut Frame,
    ) -> Result<bool, String> {
        if !self.source_alive() {
            return Err("目标窗口已关闭".into());
        }
        win::pump();
        let ok = unsafe { PrintWindow(self.host, self.dib.dc, PW_RENDERFULLCONTENT_FLAG) };
        if ok == 0 {
            return Err("PrintWindow(宿主, PW_RENDERFULLCONTENT) 返回失败".into());
        }
        let (w, h) = self.out;
        let pixels = self.dib.pixels();
        if prev.width == w && prev.height == h && !prev.changed_enough_bytes(pixels, threshold) {
            return Ok(false);
        }
        if frame.width != w || frame.height != h || frame.bgra.len() != pixels.len() {
            *frame = Frame::new(w, h, pixels.to_vec());
        } else {
            frame.bgra.copy_from_slice(pixels);
        }
        Ok(true)
    }

    /// 直接对源窗口抓一帧（`PrintWindow(源, PW_RENDERFULLCONTENT)`）。
    ///
    /// **只作为诊断/对照**：实测在 Chromium 这类 GPU 合成窗口上直接失败，
    /// 因此它不是主路径，不要拿它替换 `grab()`。
    #[allow(dead_code)]
    pub fn grab_direct_from_source(&self) -> Result<Frame, String> {
        let (w, h) = win::client_size(self.target);
        if w <= 0 || h <= 0 {
            return Err(format!("源窗口客户区尺寸异常（{w}x{h}）"));
        }
        let dib = Dib::new(w, h)?;
        let ok = unsafe { PrintWindow(self.target, dib.dc, PW_RENDERFULLCONTENT_FLAG) };
        if ok == 0 {
            return Err("PrintWindow(源窗口) 返回失败".into());
        }
        Ok(Frame::new(w, h, dib.pixels().to_vec()))
    }

    /// 屏幕 BitBlt 一块区域（诊断用；被遮挡即失效）
    #[allow(dead_code)]
    pub fn grab_screen_rect(rc: &RECT) -> Result<Frame, String> {
        let w = rc.right - rc.left;
        let h = rc.bottom - rc.top;
        let dib = Dib::new(w, h)?;
        let ok = unsafe {
            let screen = windows_sys::Win32::Graphics::Gdi::GetDC(null_mut());
            let r = BitBlt(dib.dc, 0, 0, w, h, screen, rc.left, rc.top, SRCCOPY);
            windows_sys::Win32::Graphics::Gdi::ReleaseDC(null_mut(), screen);
            r
        };
        if ok == 0 {
            return Err("屏幕 BitBlt 失败".into());
        }
        Ok(Frame::new(w, h, dib.pixels().to_vec()))
    }
}

impl Drop for ThumbnailCapture {
    fn drop(&mut self) {
        unsafe {
            if self.thumb != 0 {
                DwmUnregisterThumbnail(self.thumb);
                self.thumb = 0;
            }
            if !self.host.is_null() {
                DestroyWindow(self.host);
                self.host = null_mut();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{fit_size, validate_size};

    #[test]
    fn 预览缩放按宽度等比且不放大() {
        // 宽于上限：等比缩到 480 宽
        assert_eq!(fit_size((0, 0, 1920, 986), 480), (480, 247));
        // 窄于上限：保持原样
        assert_eq!(fit_size((0, 0, 320, 240), 480), (320, 240));
        // 不缩放
        assert_eq!(fit_size((0, 0, 1920, 986), i32::MAX), (1920, 986));
        // 极端宽扁不产生 0 高
        let (w, h) = fit_size((0, 0, 4000, 10), 480);
        assert_eq!(w, 480);
        assert!(h >= 1);
    }

    #[test]
    fn 超大画面尺寸被拒绝() {
        assert!(validate_size(20_000, 100).is_err());
        assert!(validate_size(10_000, 10_000).is_err());
        assert!(validate_size(1920, 1080).is_ok());
    }
}
