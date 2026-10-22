//! SAI2 / CLIP STUDIO PAINT 分离导航器采集。
//!
//! 导航器始终显示固定方向、完整画布的合成缩略图，因此不需要画布校准。
//! 本模块只读取窗口结构和捕获像素，不注入或操作 SAI2。

use windows_sys::Win32::Foundation::HWND;

use crate::frame::Frame;
use crate::win::{self, WindowInfo};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanvasRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone)]
pub struct NavigatorProbe {
    /// 独立导航器顶层窗口（标题通常为“导航器”或“Navigator”）。
    pub root: WindowInfo,
    /// 实际绘制导航缩略图的内部窗口。
    pub capture: WindowInfo,
}

fn is_navigator_title(title: &str) -> bool {
    let t = title.trim().to_lowercase();
    [
        "导航器",
        "導航器",
        "navigator",
        "ナビゲータ",
        "ナビゲーター",
    ]
    .iter()
    .any(|base| {
        t == *base
            || t.strip_prefix(base).is_some_and(|suffix| {
                !suffix.is_empty()
                    && suffix
                        .trim_start()
                        .chars()
                        .all(|c| c.is_ascii_digit() || c == '#')
            })
    })
}

fn is_sai_root(info: &WindowInfo) -> bool {
    info.class_name == "sflRootWindow"
        && (info.process.eq_ignore_ascii_case("sai2.exe")
            || info.process.eq_ignore_ascii_case("sai.exe"))
}

fn is_csp_window(info: &WindowInfo) -> bool {
    info.process
        .to_ascii_lowercase()
        .starts_with("clipstudiopaint")
}

fn is_supported_window(info: &WindowInfo) -> bool {
    is_sai_root(info) || is_csp_window(info)
}

/// 查找当前面积最大的受支持主窗口，用于自动选择导航器。
pub fn find_navigator_main() -> Option<WindowInfo> {
    win::list_windows()
        .into_iter()
        .filter(|w| is_supported_window(w) && !is_navigator_title(&w.title))
        .max_by_key(|w| {
            let (cw, ch) = w.client_size();
            cw as i64 * ch as i64
        })
}

/// 查找当前面积最大的 SAI2 主窗口。
pub fn find_sai2_main() -> Option<WindowInfo> {
    win::list_windows()
        .into_iter()
        .filter(|w| is_sai_root(w) && !is_navigator_title(&w.title))
        .max_by_key(|w| {
            let (cw, ch) = w.client_size();
            cw as i64 * ch as i64
        })
}

/// 从 SAI2/CSP 主窗口、画布子窗口或导航器窗口定位分离导航器。
pub fn probe(selected: HWND) -> Result<NavigatorProbe, String> {
    if !win::is_window_alive(selected) {
        return Err("绘画软件窗口已关闭或句柄无效".into());
    }

    let selected_info = win::describe(selected);
    let selected_root = win::root_of(selected);
    let selected_root_info = win::describe(selected_root);
    let pid = if selected_info.pid != 0 {
        selected_info.pid
    } else {
        selected_root_info.pid
    };
    let is_sai = is_sai_root(&selected_info) || is_sai_root(&selected_root_info);

    let root = if is_navigator_title(&selected_info.title) && is_supported_window(&selected_info) {
        selected_info
    } else if is_navigator_title(&selected_root_info.title)
        && is_supported_window(&selected_root_info)
    {
        selected_root_info
    } else {
        win::list_windows()
            .into_iter()
            .filter(|w| w.pid == pid && is_supported_window(w) && is_navigator_title(&w.title))
            .max_by_key(|w| {
                let (cw, ch) = w.client_size();
                cw as i64 * ch as i64
            })
            .ok_or_else(|| {
                if is_sai {
                    crate::lang::s(crate::lang::K::SaiNavigatorMissing).to_string()
                } else {
                    "未找到独立的“导航器”窗口。请先在 CSP 中打开导航器并保持为独立窗口。"
                        .to_string()
                }
            })?
    };

    let (rw, rh) = root.client_size();
    if rw < 120 || rh < 120 {
        return Err(format!(
            "导航器窗口太小（{rw}x{rh}）。请先放大导航器，让完整画布尽量占满窗口。"
        ));
    }

    let capture = if is_sai_root(&root) {
        // SAI2: root → 内容容器 → 缩略图绘制窗口，另有底部控件。
        let root_area = rw as i64 * rh as i64;
        win::list_child_windows(root.hwnd)
            .into_iter()
            .filter(|w| {
                let (cw, ch) = w.client_size();
                let area = cw as i64 * ch as i64;
                cw >= rw / 2
                    && ch >= rh / 2
                    && area >= root_area * 55 / 100
                    && area <= root_area * 96 / 100
            })
            .max_by_key(|w| {
                let (cw, ch) = w.client_size();
                cw as i64 * ch as i64
            })
            .ok_or_else(|| "导航器内部结构无法识别，未找到缩略图子窗口".to_string())?
    } else {
        // CSP 的导航画面直接绘制在独立顶层窗口中，没有可用的大尺寸子窗口。
        root.clone()
    };

    Ok(NavigatorProbe { root, capture })
}

fn channel_diff(a: [u8; 3], b: [u8; 3]) -> i32 {
    (a[0] as i32 - b[0] as i32)
        .abs()
        .max((a[1] as i32 - b[1] as i32).abs())
        .max((a[2] as i32 - b[2] as i32).abs())
}

fn median_rgb(samples: &mut [[u8; 3]]) -> [u8; 3] {
    let mut channels = [
        samples.iter().map(|c| c[0]).collect::<Vec<_>>(),
        samples.iter().map(|c| c[1]).collect::<Vec<_>>(),
        samples.iter().map(|c| c[2]).collect::<Vec<_>>(),
    ];
    for values in &mut channels {
        values.sort_unstable();
    }
    [
        channels[0][channels[0].len() / 2],
        channels[1][channels[1].len() / 2],
        channels[2][channels[2].len() / 2],
    ]
}

fn edge_background(frame: &Frame) -> Option<[u8; 3]> {
    let mid_y = frame.height / 2;
    let mut samples: Vec<[u8; 3]> = Vec::new();
    for y in [
        frame.height / 4,
        mid_y.saturating_sub(8),
        mid_y,
        mid_y.saturating_add(8).min(frame.height - 1),
        frame.height * 3 / 4,
    ] {
        for x in [
            2,
            frame.width / 40,
            frame.width - 3,
            frame.width - frame.width / 40,
        ] {
            if let Some(c) = frame.get(x.clamp(0, frame.width - 1), y.clamp(0, frame.height - 1)) {
                samples.push(c);
            }
        }
    }
    (!samples.is_empty()).then(|| median_rgb(&mut samples))
}

fn chrome_background(frame: &Frame) -> [u8; 3] {
    let mut samples = Vec::with_capacity(4);
    for (x, y) in [
        (0, 0),
        (frame.width - 1, 0),
        (0, frame.height - 1),
        (frame.width - 1, frame.height - 1),
    ] {
        if let Some(c) = frame.get(x, y) {
            samples.push(c);
        }
    }
    median_rgb(&mut samples)
}

fn longest_true_run(values: &[bool]) -> Option<(i32, i32)> {
    let mut best: Option<(i32, i32)> = None;
    let mut start: Option<usize> = None;
    for (i, value) in values
        .iter()
        .copied()
        .chain(std::iter::once(false))
        .enumerate()
    {
        match (value, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                let run = (s as i32, (i - 1) as i32);
                if best.map_or(true, |(a, b)| run.1 - run.0 > b - a) {
                    best = Some(run);
                }
                start = None;
            }
            _ => {}
        }
    }
    best
}

fn detect_with_background(
    frame: &Frame,
    background: [u8; 3],
    threshold: i32,
    pad_right_edge: bool,
) -> Result<CanvasRect, String> {
    let differs = |x: i32, y: i32| -> bool {
        frame
            .get(x, y)
            .is_some_and(|c| channel_diff(c, background) > threshold)
    };

    // 排除导航器最外圈 1~2px 的立体边框/阴影。
    let pad = (frame.width.min(frame.height) / 150).clamp(4, 10);
    let scan_left = pad;
    let scan_right = if pad_right_edge {
        frame.width - pad
    } else {
        frame.width
    };

    let mut col_hits = vec![0i32; frame.width as usize];
    for x in scan_left..scan_right {
        for y in 0..frame.height {
            if differs(x, y) {
                col_hits[x as usize] += 1;
            }
        }
    }
    let col_threshold = (frame.height as f32 * 0.40) as i32;
    let col_mask: Vec<bool> = col_hits.iter().map(|n| *n >= col_threshold).collect();
    let (left, right) =
        longest_true_run(&col_mask).ok_or_else(|| "导航器中未识别到画布列".to_string())?;
    if right <= left + 20 {
        return Err("导航器中识别到的画布宽度异常".into());
    }

    let mut row_hits = vec![0i32; frame.height as usize];
    let row_span = (right - left + 1).max(1);
    for y in 0..frame.height {
        for x in left..=right {
            if differs(x, y) {
                row_hits[y as usize] += 1;
            }
        }
    }
    let row_threshold = (row_span as f32 * 0.50) as i32;
    let row_mask: Vec<bool> = row_hits.iter().map(|n| *n >= row_threshold).collect();
    let (top, bottom) =
        longest_true_run(&row_mask).ok_or_else(|| "导航器中未识别到画布行".to_string())?;
    if bottom <= top + 20 {
        return Err("导航器中识别到的画布高度异常".into());
    }

    // 边缘可能有 1px 抗锯齿/描边，内缩 1px，避免把导航器边框写进成片。
    let x = (left + 1).min(right);
    let y = top;
    let width = (right - x).max(1);
    let (y, height) = trim_unstable_rows(frame, x, y, width, (bottom - y + 1).max(1));
    if width < frame.width / 2 || height < frame.height / 2 {
        return Err(format!(
            "自动定位的画布区域过小（{width}x{height}）。请放大导航器后重试。"
        ));
    }
    Ok(CanvasRect {
        x,
        y,
        width,
        height,
    })
}

fn row_difference(frame: &Frame, y1: i32, y2: i32, x: i32, width: i32) -> f32 {
    let mut sum = 0u64;
    let mut count = 0u64;
    for col in x..x + width {
        if let (Some(a), Some(b)) = (frame.get(col, y1), frame.get(col, y2)) {
            sum += channel_diff(a, b) as u64;
            count += 1;
        }
    }
    if count == 0 {
        0.0
    } else {
        sum as f32 / count as f32
    }
}

/// 去掉检测结果边缘整行出现的分隔线/阴影，最多清理 4 行。
fn trim_unstable_rows(
    frame: &Frame,
    x: i32,
    mut y: i32,
    width: i32,
    mut height: i32,
) -> (i32, i32) {
    for _ in 0..4 {
        if height <= 80 || row_difference(frame, y, y + 1, x, width) <= 24.0 {
            break;
        }
        y += 1;
        height -= 1;
    }
    for _ in 0..4 {
        if height <= 80 || row_difference(frame, y + height - 1, y + height - 2, x, width) <= 24.0 {
            break;
        }
        height -= 1;
    }
    (y, height)
}

/// 从导航器抓图中自动定位实际画布。
///
/// 首选“导航器边缘背景色”差分，适合画布四周有留白的常见情况。若画布与
/// 右侧背景同为白色并几乎填满窗口，则退回“标题栏/外框颜色”差分。
pub fn detect_canvas_rect(frame: &Frame) -> Result<CanvasRect, String> {
    if frame.width < 80 || frame.height < 80 {
        return Err("导航器抓图太小，无法定位画布区域".into());
    }

    let edge = edge_background(frame).ok_or_else(|| "无法读取导航器背景色".to_string())?;
    let first = detect_with_background(frame, edge, 32, true);
    if first.is_ok() {
        return first;
    }
    let chrome = chrome_background(frame);
    let second = detect_with_background(frame, chrome, 20, false);
    if let Ok(rect) = second {
        crate::logline!(
            "[navigator] 画布检测使用外框回退 bg=({},{},{}) rect=({},{},{},{})",
            chrome[0],
            chrome[1],
            chrome[2],
            rect.x,
            rect.y,
            rect.width,
            rect.height
        );
        return Ok(rect);
    }
    first
}

pub fn crop_canvas(frame: &Frame, rect: CanvasRect) -> Result<Frame, String> {
    frame.crop(rect.x, rect.y, rect.width, rect.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_nav(w: i32, h: i32, page: CanvasRect, chrome: [u8; 3]) -> Frame {
        let mut frame = Frame::new(w, h, vec![0u8; (w * h * 4) as usize]);
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let rgb = if x >= page.x
                    && y >= page.y
                    && x < page.x + page.width
                    && y < page.y + page.height
                {
                    [245, 245, 245]
                } else {
                    chrome
                };
                frame.bgra[i] = rgb[2];
                frame.bgra[i + 1] = rgb[1];
                frame.bgra[i + 2] = rgb[0];
                frame.bgra[i + 3] = 255;
            }
        }
        frame
    }

    #[test]
    fn 自动裁出居中的导航画布() {
        let page = CanvasRect {
            x: 80,
            y: 24,
            width: 600,
            height: 780,
        };
        let frame = synthetic_nav(755, 805, page, [22, 22, 22]);
        let got = detect_canvas_rect(&frame).unwrap();
        assert!((got.x - 81).abs() <= 1, "left={}", got.x);
        assert!((got.y - 24).abs() <= 1, "top={}", got.y);
        assert!((got.width - 598).abs() <= 2, "width={}", got.width);
        assert!((got.height - 780).abs() <= 2, "height={}", got.height);
    }

    #[test]
    fn 白色画布填满导航器时使用外框回退() {
        let page = CanvasRect {
            x: 4,
            y: 23,
            width: 505,
            height: 654,
        };
        let frame = synthetic_nav(509, 677, page, [48, 48, 48]);
        let got = detect_canvas_rect(&frame).unwrap();
        assert_eq!(got.x, 5);
        assert_eq!(got.y, 23);
        assert_eq!(got.width, 503);
        assert_eq!(got.height, 654);
    }

    #[test]
    fn csp中央画布不会被视口指示框缩小() {
        let page = CanvasRect {
            x: 156,
            y: 20,
            width: 398,
            height: 530,
        };
        let mut frame = synthetic_nav(711, 598, page, [45, 45, 45]);
        // 红色视口框位于页面内部，不能成为画布边界。
        for y in 55..185 {
            for x in 50..340 {
                if x == 50 || x == 339 || y == 55 || y == 184 {
                    let i = ((y * frame.width + x) * 4) as usize;
                    frame.bgra[i..i + 3].copy_from_slice(&[40, 40, 220]);
                }
            }
        }
        let got = detect_canvas_rect(&frame).unwrap();
        assert!((got.x - 157).abs() <= 1);
        assert!((got.y - 20).abs() <= 1);
        assert!((got.width - 396).abs() <= 2);
        assert!((got.height - 530).abs() <= 2);
    }

    #[test]
    fn 识别csp进程和导航器标题() {
        let info = WindowInfo {
            hwnd: std::ptr::null_mut(),
            title: "导航器".into(),
            class_name: "csp-window".into(),
            pid: 1,
            process: "clipstudiopaint504.exe".into(),
            rect: unsafe { std::mem::zeroed() },
            client: unsafe { std::mem::zeroed() },
            minimized: false,
        };
        assert!(is_csp_window(&info));
        assert!(is_supported_window(&info));
        assert!(is_navigator_title(&info.title));
        assert!(is_navigator_title("Navigator 2"));
        assert!(!is_navigator_title("Navigator settings"));
    }
}
