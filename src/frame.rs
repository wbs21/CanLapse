//! 一帧画面（BGRA，行序从上到下）与帧间差分。
//!
//! 差分语义按产品需求定义：`changed_ratio` 返回"变化像素占总像素的比例"（0.0~1.0），
//! 调用方拿它与「像素差异阈值」（UI 上的 0.00~0.05）比较，超过才保存这一帧。
//! 静止画面因此不会产生帧 —— 这就是"缩时"的来源。

/// 单像素三通道差之和超过这个值，才算"这个像素变了"。
/// 取 24 是为了滤掉编码/合成的微小抖动，避免噪点被当成画面变化。
const PIXEL_CHANGED_DELTA: i32 = 24;

#[derive(Clone)]
pub struct Frame {
    pub width: i32,
    pub height: i32,
    /// BGRA，长度为 `width * height * 4`，行序从上到下（top-down DIB 的原样内存）
    pub bgra: Vec<u8>,
}

impl Frame {
    pub fn new(width: i32, height: i32, bgra: Vec<u8>) -> Self {
        debug_assert_eq!(bgra.len(), (width as usize) * (height as usize) * 4);
        Self { width, height, bgra }
    }

    pub fn byte_len(&self) -> usize {
        self.bgra.len()
    }

    /// 与另一帧相比，变化像素所占比例（0.0 ~ 1.0）。
    /// 尺寸不同视为"整幅都变了"，返回 1.0。
    pub fn changed_ratio(&self, other: &Frame) -> f32 {
        if self.width != other.width || self.height != other.height {
            return 1.0;
        }
        let mut changed: usize = 0;
        let mut total: usize = 0;
        let mut i = 0usize;
        while i + 3 < self.bgra.len() {
            total += 1;
            let d = (self.bgra[i] as i32 - other.bgra[i] as i32).abs()
                + (self.bgra[i + 1] as i32 - other.bgra[i + 1] as i32).abs()
                + (self.bgra[i + 2] as i32 - other.bgra[i + 2] as i32).abs();
            if d > PIXEL_CHANGED_DELTA {
                changed += 1;
            }
            i += 4;
        }
        if total == 0 {
            return 0.0;
        }
        changed as f32 / total as f32
    }

    /// 画面是否几乎全黑/恒定 —— 用于抓帧自检告警（见 ADR 0002 的"依赖实现行为"风险）。
    pub fn is_blank(&self) -> bool {
        let mut first: Option<[u8; 3]> = None;
        let mut i = 0usize;
        while i + 3 < self.bgra.len() {
            let px = [self.bgra[i], self.bgra[i + 1], self.bgra[i + 2]];
            match first {
                None => first = Some(px),
                Some(f) => {
                    if (px[0] as i32 - f[0] as i32).abs() > 6
                        || (px[1] as i32 - f[1] as i32).abs() > 6
                        || (px[2] as i32 - f[2] as i32).abs() > 6
                    {
                        return false;
                    }
                }
            }
            i += 4;
        }
        true
    }

    /// 转成 RGBA（给 PNG 编码用）
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.bgra.len());
        for px in self.bgra.chunks_exact(4) {
            out.push(px[2]);
            out.push(px[1]);
            out.push(px[0]);
            out.push(255);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: i32, h: i32, rgb: [u8; 3]) -> Frame {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            v.push(rgb[2]);
            v.push(rgb[1]);
            v.push(rgb[0]);
            v.push(255);
        }
        Frame::new(w, h, v)
    }

    #[test]
    fn 相同画面差异为零() {
        let a = solid(8, 8, [10, 20, 30]);
        let b = solid(8, 8, [10, 20, 30]);
        assert_eq!(a.changed_ratio(&b), 0.0);
    }

    #[test]
    fn 尺寸不同视为整幅变化() {
        let a = solid(8, 8, [0, 0, 0]);
        let b = solid(4, 4, [0, 0, 0]);
        assert_eq!(a.changed_ratio(&b), 1.0);
    }

    #[test]
    fn 微小抖动不算变化() {
        // 单通道差 5（三通道合计 5）< 阈值 24，属于压缩/合成抖动，不应判为变化
        let a = solid(8, 8, [100, 100, 100]);
        let b = solid(8, 8, [105, 100, 100]);
        assert_eq!(a.changed_ratio(&b), 0.0);
    }

    #[test]
    fn 明显变化被识别且比例正确() {
        // 4x4 = 16 像素，改 4 个 -> 25%
        let a = solid(4, 4, [0, 0, 0]);
        let mut v = a.bgra.clone();
        for i in 0..4 {
            v[i * 4] = 255;
            v[i * 4 + 1] = 255;
            v[i * 4 + 2] = 255;
        }
        let b = Frame::new(4, 4, v);
        assert!((a.changed_ratio(&b) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn 纯色被识别为空画面() {
        assert!(solid(8, 8, [0, 0, 0]).is_blank());
        assert!(solid(8, 8, [123, 45, 67]).is_blank());
        let mut v = solid(8, 8, [0, 0, 0]).bgra;
        v[40] = 255; // 第 10 个像素的 B 通道
        assert!(!Frame::new(8, 8, v).is_blank());
    }

    #[test]
    fn bgra转rgba通道顺序正确() {
        let f = solid(1, 1, [10, 20, 30]); // 传入 RGB
        let rgba = f.to_rgba();
        assert_eq!(rgba, vec![10, 20, 30, 255]);
    }
}
