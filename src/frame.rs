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

    /// 取一个像素的 RGB（越界返回 None）。测试、UI 预览、边缘检测都会用到。
    pub fn get(&self, x: i32, y: i32) -> Option<[u8; 3]> {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return None;
        }
        let i = ((y * self.width + x) * 4) as usize;
        Some([self.bgra[i + 2], self.bgra[i + 1], self.bgra[i]])
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

    #[inline]
    fn channel(&self, x: i32, y: i32, c: usize) -> f64 {
        let i = ((y * self.width + x) * 4) as usize + c;
        self.bgra[i] as f64
    }

    /// 缩放（只处理 BGRA，alpha 一律写 255）。
    ///
    /// * 缩小：**面积平均**（box/area filter）—— 对降采样比双线性更合适，不会产生锯齿与振铃；
    /// * 放大：双线性。
    ///
    /// 之所以不用 `image` crate 的 Lanczos3：这里只在"录制中途画布窗口尺寸变了"时才会走到，
    /// 属于罕见路径，不值得为它引入一整套图像库依赖。若日后确实需要更高质量，
    /// 换成 `image::imageops::resize(.., Lanczos3)` 即可，接口不变。
    pub fn resize_to(&self, new_w: i32, new_h: i32) -> Frame {
        if new_w == self.width && new_h == self.height {
            return self.clone();
        }
        let (sw, sh) = (self.width.max(1), self.height.max(1));
        let nw = new_w.max(1);
        let nh = new_h.max(1);
        let mut out = vec![0u8; (nw * nh * 4) as usize];
        // 目标像素面积更小 => 缩小
        let shrinking = (nw as i64) * (sh as i64) < (sw as i64) * (nh as i64);

        for y in 0..nh {
            for x in 0..nw {
                let o = ((y * nw + x) * 4) as usize;
                if shrinking {
                    let x0 = (x as i64 * sw as i64 / nw as i64).clamp(0, (sw - 1) as i64) as i32;
                    let x1 = (((x + 1) as i64 * sw as i64 / nw as i64).max(x0 as i64 + 1))
                        .clamp(1, sw as i64) as i32;
                    let y0 = (y as i64 * sh as i64 / nh as i64).clamp(0, (sh - 1) as i64) as i32;
                    let y1 = (((y + 1) as i64 * sh as i64 / nh as i64).max(y0 as i64 + 1))
                        .clamp(1, sh as i64) as i32;
                    let (mut acc, mut n) = ([0f64; 3], 0f64);
                    for sy in y0..y1 {
                        for sx in x0..x1 {
                            for c in 0..3 {
                                acc[c] += self.channel(sx, sy, c);
                            }
                            n += 1.0;
                        }
                    }
                    let n = n.max(1.0);
                    for c in 0..3 {
                        out[o + c] = (acc[c] / n).round().clamp(0.0, 255.0) as u8;
                    }
                } else {
                    let fx = ((x as f64 + 0.5) * sw as f64 / nw as f64 - 0.5).max(0.0);
                    let fy = ((y as f64 + 0.5) * sh as f64 / nh as f64 - 0.5).max(0.0);
                    let x0 = (fx.floor() as i32).clamp(0, sw - 1);
                    let y0 = (fy.floor() as i32).clamp(0, sh - 1);
                    let x1 = (x0 + 1).min(sw - 1);
                    let y1 = (y0 + 1).min(sh - 1);
                    let tx = fx - x0 as f64;
                    let ty = fy - y0 as f64;
                    for c in 0..3 {
                        let top = self.channel(x0, y0, c) * (1.0 - tx) + self.channel(x1, y0, c) * tx;
                        let bot = self.channel(x0, y1, c) * (1.0 - tx) + self.channel(x1, y1, c) * tx;
                        out[o + c] = (top * (1.0 - ty) + bot * ty).round().clamp(0.0, 255.0) as u8;
                    }
                }
                out[o + 3] = 255;
            }
        }
        Frame::new(nw, nh, out)
    }

    /// 等比缩放 + 居中 + 填充，产出恰好 `target_w × target_h` 的一帧。
    ///
    /// 这是需求里「分辨率统一」的核心：**第一张保存的帧决定视频分辨率**，
    /// 之后所有帧都适配到该尺寸 —— 等比、居中、四周填充（默认黑边），
    /// 保留全部画面、不裁切、不变形。
    ///
    /// `fill_rgb` 是填充色（黑边用 `[0,0,0]`）。尺寸本来就一致时直接返回克隆，不做任何缩放。
    pub fn fit_into(&self, target_w: i32, target_h: i32, fill_rgb: [u8; 3]) -> Frame {
        if self.width == target_w && self.height == target_h {
            return self.clone();
        }
        let tw = target_w.max(1);
        let th = target_h.max(1);
        let scale = f64::min(tw as f64 / self.width as f64, th as f64 / self.height as f64);
        let new_w = ((self.width as f64 * scale).round() as i32).clamp(1, tw);
        let new_h = ((self.height as f64 * scale).round() as i32).clamp(1, th);
        let scaled = self.resize_to(new_w, new_h);

        // 先铺满填充色，再把缩放后的画面贴到正中
        let mut canvas = vec![0u8; (tw * th * 4) as usize];
        for px in canvas.chunks_exact_mut(4) {
            px[0] = fill_rgb[2];
            px[1] = fill_rgb[1];
            px[2] = fill_rgb[0];
            px[3] = 255;
        }
        let off_x = (tw - new_w) / 2;
        let off_y = (th - new_h) / 2;
        let row_bytes = (new_w * 4) as usize;
        for y in 0..new_h {
            let src = (y * new_w * 4) as usize;
            let dst = (((y + off_y) * tw + off_x) * 4) as usize;
            canvas[dst..dst + row_bytes]
                .copy_from_slice(&scaled.bgra[src..src + row_bytes]);
        }
        Frame::new(tw, th, canvas)
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

    // ---- 分辨率统一（等比 + 居中 + 填充）----

    #[test]
    fn 尺寸一致时适配不做任何改动() {
        let a = solid(8, 8, [10, 20, 30]);
        let b = a.fit_into(8, 8, [0, 0, 0]);
        assert_eq!(a.bgra, b.bgra);
    }

    #[test]
    fn 宽幅画面适配到方形时上下补黑边() {
        // 16x4 -> 8x8：scale=min(8/16, 8/4)=0.5 -> 内容 8x2，y 偏移 (8-2)/2=3
        let src = solid(16, 4, [255, 0, 0]);
        let out = src.fit_into(8, 8, [0, 0, 0]);
        assert_eq!((out.width, out.height), (8, 8));
        assert_eq!(out.get(4, 0), Some([0, 0, 0]), "第 0 行应是黑边");
        assert_eq!(out.get(4, 4), Some([255, 0, 0]), "中间行应是内容");
        assert_eq!(out.get(4, 7), Some([0, 0, 0]), "最后一行应是黑边");
    }

    #[test]
    fn 高幅画面适配到方形时左右补黑边() {
        // 4x16 -> 8x8：scale=0.5 -> 内容 2x8，x 偏移 3
        let src = solid(4, 16, [0, 255, 0]);
        let out = src.fit_into(8, 8, [0, 0, 0]);
        assert_eq!(out.get(0, 4), Some([0, 0, 0]));
        assert_eq!(out.get(4, 4), Some([0, 255, 0]));
        assert_eq!(out.get(7, 4), Some([0, 0, 0]));
    }

    #[test]
    fn 放大到更大基准尺寸也不变形() {
        // 4x2 -> 8x8：scale=min(2,4)=2 -> 内容 8x4，上下各 2 行黑边
        let src = solid(4, 2, [1, 2, 3]);
        let out = src.fit_into(8, 8, [0, 0, 0]);
        assert_eq!(out.get(4, 0), Some([0, 0, 0]));
        assert_eq!(out.get(4, 4), Some([1, 2, 3]));
        assert_eq!(out.get(4, 7), Some([0, 0, 0]));
    }

    #[test]
    fn 缩小用面积平均而不是抽样() {
        // 4x4 里左上 2x2 是白的，其余是黑的；缩到 2x2 时
        // 左上目标格覆盖 4 个白像素 -> 应为纯白（若是最近邻抽样也可能对，
        // 但右上格覆盖 4 个黑像素必须为纯黑，能抓出"平均把黑白混在中格"的实现错误）
        let mut v = vec![0u8; 4 * 4 * 4];
        for px in v.chunks_exact_mut(4) {
            px[3] = 255;
        }
        for y in 0..2 {
            for x in 0..2 {
                let o = ((y * 4 + x) * 4) as usize;
                v[o] = 255;
                v[o + 1] = 255;
                v[o + 2] = 255;
            }
        }
        let out = Frame::new(4, 4, v).resize_to(2, 2);
        assert_eq!(out.get(0, 0), Some([255, 255, 255]));
        assert_eq!(out.get(1, 0), Some([0, 0, 0]));
        assert_eq!(out.get(0, 1), Some([0, 0, 0]));
        assert_eq!(out.get(1, 1), Some([0, 0, 0]));
    }

    #[test]
    fn 填充色可配置() {
        let src = solid(4, 4, [255, 255, 255]);
        let out = src.fit_into(8, 4, [0, 0, 255]); // 用蓝色填充
        assert_eq!(out.get(0, 2), Some([0, 0, 255]));
        assert_eq!(out.get(4, 2), Some([255, 255, 255]));
    }
}
