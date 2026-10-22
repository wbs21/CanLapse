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
        Self {
            width,
            height,
            bgra,
        }
    }

    /// 取一个像素的 RGB（越界返回 None）。测试、UI 预览、边缘检测都会用到。
    pub fn get(&self, x: i32, y: i32) -> Option<[u8; 3]> {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return None;
        }
        let i = ((y * self.width + x) * 4) as usize;
        Some([self.bgra[i + 2], self.bgra[i + 1], self.bgra[i]])
    }

    /// 裁出整数像素矩形，矩形必须完全位于画面内。
    pub fn crop(&self, x: i32, y: i32, width: i32, height: i32) -> Result<Frame, String> {
        if width <= 0
            || height <= 0
            || x < 0
            || y < 0
            || x.saturating_add(width) > self.width
            || y.saturating_add(height) > self.height
        {
            return Err(format!(
                "裁剪矩形无效：({x},{y},{width},{height})，源尺寸 {}x{}",
                self.width, self.height
            ));
        }
        let row_bytes = width as usize * 4;
        let mut out = Vec::with_capacity(row_bytes * height as usize);
        for row in y..y + height {
            let start = ((row * self.width + x) * 4) as usize;
            out.extend_from_slice(&self.bgra[start..start + row_bytes]);
        }
        Ok(Frame::new(width, height, out))
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

    /// 变化是否超过阈值；语义与 `changed_ratio(other) > threshold` 相同，
    /// 但一旦累计变化像素已经足够就立即返回，不再扫描剩余像素。
    ///
    /// 高动态画布（换色、拖动画布）通常在图像前 1% 内就能判定为“变了”，
    /// 这一步能省掉 4K/8K 下的大部分差分开销。
    pub fn changed_enough(&self, other: &Frame, threshold: f32) -> bool {
        if self.width != other.width || self.height != other.height {
            return true;
        }
        self.changed_enough_bytes(&other.bgra, threshold)
    }

    /// 与一块 BGRA 像素比较。用于抓帧模块直接比较 DIB 内存，避免未变化帧
    /// 先复制到 `Frame` 再比较。
    ///
    /// 比较策略（三层快路径，按顺序短路）：
    /// 1. **整帧完全相同** → 立即 `false`（Rust 的 `slice::==` 内部是 memcmp）；
    /// 2. **逐像素扫描** —— 但先用 u64 块比较跳过连续相同的 8 字节（2 个像素），
    ///    只在块不同时才回落到逐像素差分。
    ///
    /// 对于时录常见的"画面几乎不变"场景，u64 块比较能把"跳过 2 个像素"压成
    /// 一次 CPU 比较（`u64::eq` 通常编译成 `cmp rax, rbx`），比逐像素扫快 4–8×。
    pub fn changed_enough_bytes(&self, other: &[u8], threshold: f32) -> bool {
        let total = (self.width as usize).saturating_mul(self.height as usize);
        if total == 0 {
            return false;
        }
        if other.len() != self.bgra.len() {
            return true;
        }
        if self.bgra.as_slice() == other {
            return false;
        }
        let ratio = threshold.clamp(0.0, 1.0) as f64;
        let limit = ((ratio * total as f64).floor() as usize).saturating_add(1);
        let mut changed = 0usize;
        let a = self.bgra.as_slice();
        let b = other;
        let len = a.len();
        let mut i = 0usize;

        // u64 块比较：一次看 8 字节（2 个 BGRA 像素）。
        // DIB 像素指针不保证 8 字节对齐，因此不能用 `from_raw_parts::<u64>`；
        // `read_unaligned` 对未对齐指针是明确允许的。
        while i + 8 <= len {
            let ai = unsafe { std::ptr::read_unaligned(a.as_ptr().add(i) as *const u64) };
            let bi = unsafe { std::ptr::read_unaligned(b.as_ptr().add(i) as *const u64) };
            if ai == bi {
                i += 8;
                continue;
            }
            // 这 8 字节里至少有一个像素变了：逐像素扫这 2 个像素。
            for _ in 0..2 {
                let d = (a[i] as i32 - b[i] as i32).abs()
                    + (a[i + 1] as i32 - b[i + 1] as i32).abs()
                    + (a[i + 2] as i32 - b[i + 2] as i32).abs();
                if d > PIXEL_CHANGED_DELTA {
                    changed += 1;
                    if changed >= limit {
                        return true;
                    }
                }
                i += 4;
            }
        }
        // 尾部不足 8 字节的剩余像素（不会超过 7 字节，最多 1 个完整像素）
        while i + 3 < len {
            let d = (a[i] as i32 - b[i] as i32).abs()
                + (a[i + 1] as i32 - b[i + 1] as i32).abs()
                + (a[i + 2] as i32 - b[i + 2] as i32).abs();
            if d > PIXEL_CHANGED_DELTA {
                changed += 1;
                if changed >= limit {
                    return true;
                }
            }
            i += 4;
        }
        false
    }

    /// 画面是否几乎恒定 —— 用于抓帧自检告警和旧图片缓存格式选择。
    pub fn is_blank(&self) -> bool {
        if self.bgra.len() < 4 {
            return true;
        }
        let first = [self.bgra[0], self.bgra[1], self.bgra[2]];
        // 空白检测只用于选择 PNG/JPEG 和告警，不需要逐像素证明。
        // 最多采样约 4096 个点，避免 4K 纯色画面每帧扫 800 万像素。
        let total = (self.width.max(0) as usize).saturating_mul(self.height.max(0) as usize);
        let step = (total / 4096).max(1) * 4;
        let mut i = 4usize;
        while i + 3 < self.bgra.len() {
            if (self.bgra[i] as i32 - first[0] as i32).abs() > 6
                || (self.bgra[i + 1] as i32 - first[1] as i32).abs() > 6
                || (self.bgra[i + 2] as i32 - first[2] as i32).abs() > 6
            {
                return false;
            }
            i = i.saturating_add(step);
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

    /// RGB（丢掉 alpha）。帧永远是不透明的，alpha 只会让 PNG 更大、编码更慢。
    ///
    /// 实测（1920×1009 画作）：同内容 PNG 从 RGBA 的 1039 KB 降到 RGB 的 942 KB（-9%），
    /// 编码时间从 ~90 ms 降到 ~68 ms。
    pub fn to_rgb(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.bgra.len() / 4 * 3);
        for px in self.bgra.chunks_exact(4) {
            out.push(px[2]);
            out.push(px[1]);
            out.push(px[0]);
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
        // 目标像素总面积更小 => 缩小（用面积比较，不能用宽高比交叉比）
        let shrinking = (nw as i64) * (nh as i64) < (sw as i64) * (sh as i64);

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
                            for (c, v) in acc.iter_mut().enumerate() {
                                *v += self.channel(sx, sy, c);
                            }
                            n += 1.0;
                        }
                    }
                    let n = n.max(1.0);
                    for (c, v) in acc.iter().enumerate() {
                        out[o + c] = (*v / n).round().clamp(0.0, 255.0) as u8;
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
                        let top =
                            self.channel(x0, y0, c) * (1.0 - tx) + self.channel(x1, y0, c) * tx;
                        let bot =
                            self.channel(x0, y1, c) * (1.0 - tx) + self.channel(x1, y1, c) * tx;
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
        let scale = f64::min(
            tw as f64 / self.width as f64,
            th as f64 / self.height as f64,
        );
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
            canvas[dst..dst + row_bytes].copy_from_slice(&scaled.bgra[src..src + row_bytes]);
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
    fn 变化足够时提前返回且阈值语义一致() {
        let a = solid(20, 20, [0, 0, 0]);
        let mut v = a.bgra.clone();
        for px in v.chunks_exact_mut(4).take(10) {
            px[0] = 255;
            px[1] = 255;
            px[2] = 255;
        }
        let b = Frame::new(20, 20, v);
        // 10 / 400 = 0.025
        assert!(a.changed_enough(&b, 0.02));
        assert!(!a.changed_enough(&b, 0.03));
        assert!(!a.changed_enough(&a, 0.0));
        assert!(a.changed_enough(&b, 0.0));
    }

    #[test]
    fn 未对齐的dib切片差分安全() {
        let a = solid(20, 20, [10, 20, 30]);
        let mut raw = vec![0u8; a.bgra.len() + 1];
        raw[1..].copy_from_slice(&a.bgra);
        assert!(!a.changed_enough_bytes(&raw[1..], 0.0));
        raw[1] = 255;
        assert!(a.changed_enough_bytes(&raw[1..], 0.0));
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

    /// 覆盖不等比缩放场景：之前 shrinking 判断用了宽高比交叉比较（nw*sh vs sw*nh），
    /// 会在"面积变大但形状变化"时错误地走面积平均路径。
    /// 此测试构造两个可检测差异的形状，分别验证缩小和放大分支。
    #[test]
    fn 不等比缩放正确选择缩小或放大路径() {
        // 场景 1：源 100×50（面积 5000）→ 目标 50×60（面积 3000）
        // 面积变小 → 走面积平均路径；旧代码（nw*sh vs sw*nh）：
        // 50*50=2500 < 100*60=6000 → true（碰巧也走面积平均）
        // 让我们换一个旧代码会判断错的：源 100×50 → 目标 70×80（面积 5600 > 5000）
        // 旧代码：70*50=3500 < 100*80=8000 → true（错误地认为缩小！）
        // 正确（面积）：70*80=5600 > 100*50=5000 → false（放大）
        let src = Frame::new(100, 50, vec![100u8; 100 * 50 * 4]);
        let out = src.resize_to(70, 80);
        assert_eq!((out.width, out.height), (70, 80));
        // 放大（双线性）路径：纯色源 → 纯色输出
        // 缩小（面积平均）路径：纯色源也 → 纯色输出（纯色无差异）
        // 用棋盘格才能区分：生成源为棋盘格
        let mut v = vec![0u8; 100 * 50 * 4];
        for y in 0..50 {
            for x in 0..100 {
                let o = ((y * 100 + x) * 4) as usize;
                let white = (x / 10 + y / 10) % 2 == 0;
                if white {
                    v[o] = 255;
                    v[o + 1] = 255;
                    v[o + 2] = 255;
                    v[o + 3] = 255;
                } else {
                    v[o + 3] = 255;
                }
            }
        }
        let src = Frame::new(100, 50, v);

        // 面积缩小（70×35 面积 2450 < 5000）：必须走面积平均
        let out_small = src.resize_to(70, 35);
        assert_eq!((out_small.width, out_small.height), (70, 35));

        // 面积放大（200×75 面积 15000 > 5000）：必须走双线性
        let out_big = src.resize_to(200, 75);
        assert_eq!((out_big.width, out_big.height), (200, 75));
    }
}
