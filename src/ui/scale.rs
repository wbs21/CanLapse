//! 主窗口的 DPI 缩放。
//!
//! 程序声明的是 PerMonitorV2：Windows 不会再帮我们做位图拉伸，所以布局常量、
//! 字体高度、悬浮条和弹窗尺寸都必须自己按 DPI 换算。这里集中放换算函数，
//! 避免每个控件各自写一份 `dpi / 96`。

use std::sync::atomic::{AtomicU32, Ordering};

static DPI: AtomicU32 = AtomicU32::new(96);

/// 设置当前窗口所在显示器的 DPI（96 = 100%）。
pub fn set(dpi: u32) {
    DPI.store(dpi.clamp(48, 480), Ordering::Relaxed);
}

pub fn dpi() -> u32 {
    DPI.load(Ordering::Relaxed)
}

/// 把 96 DPI 下的逻辑像素换算成当前显示器的物理像素。
pub fn px(value: i32) -> i32 {
    let dpi = dpi() as i64;
    ((value as i64 * dpi + 48) / 96) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 按显示器dpi换算() {
        set(96);
        assert_eq!(px(330), 330);
        set(144);
        assert_eq!(px(11), 17);
        assert_eq!(px(330), 495);
        set(96);
    }
}
