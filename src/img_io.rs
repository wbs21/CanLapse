//! 帧缓存读写：**有内容用 JPEG（q85），近空白用 PNG**。
//!
//! 为什么不全用 PNG：PNG 是无损压缩，对绘画内容压缩率有限（1920×1009 画作约 1 MB/帧）。
//! 实测同内容 JPEG q85 约 200 KB，是 PNG 的 1/5，编码还快 8 倍；
//! 而对**纯色/近空白**的帧 JPEG 反而更大（10 KB vs 5 KB），所以这类帧保留 PNG。
//!
//! 读回时按扩展名分派，统一转成 RGBA 交给编码器；旧会话（全是 PNG）照常可读。

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use crate::frame::Frame;

/// 帧缓存的 JPEG 质量。q85 在"体积/细线振铃"之间比较平衡（q80 更小但线条边缘可见抖动）。
pub const JPEG_QUALITY: u8 = 85;

pub fn write_png(path: &Path, frame: &Frame) -> std::io::Result<()> {
    let rgb = frame.to_rgb();
    let file = File::create(path)?;
    let w = BufWriter::new(file);
    let mut enc = png::Encoder::new(w, frame.width as u32, frame.height as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header()?;
    writer.write_image_data(&rgb)?;
    writer.finish()?;
    Ok(())
}

/// 写 JPEG。先编码到内存再落盘：编码失败时不会留下半截文件。
pub fn write_jpeg(path: &Path, frame: &Frame) -> Result<(), String> {
    let rgb = frame.to_rgb();
    let mut out: Vec<u8> = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
    enc.encode(
        &rgb,
        frame.width as u32,
        frame.height as u32,
        image::ExtendedColorType::Rgb8,
    )
    .map_err(|e| format!("JPEG 编码失败: {e}"))?;
    std::fs::write(path, out).map_err(|e| format!("写入 {} 失败: {e}", path.display()))
}

/// 读回任意格式的帧为 RGBA（重新导出时用；OpenH264 需要 YUV420，转换在编码模块里做）
pub fn read_image_rgba(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if ext == "jpg" || ext == "jpeg" {
        read_jpeg_rgba(path)
    } else {
        read_png_rgba(path)
    }
}

fn read_jpeg_rgba(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("打开 {} 失败: {e}", path.display()))?;
    let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg)
        .map_err(|e| format!("解析 JPEG 失败: {e}"))?;
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    Ok((w, h, rgba.into_raw()))
}

/// 读回 PNG 为 RGBA（旧会话 / 空白帧；调色板、低位深统一展开）
pub fn read_png_rgba(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    let file = File::open(path).map_err(|e| format!("打开 {} 失败: {e}", path.display()))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|e| format!("解析 PNG 失败: {e}"))?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| "无法确定 PNG 输出缓冲大小".to_string())?;
    let mut buf = vec![0u8; size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("读取 PNG 帧失败: {e}"))?;
    buf.truncate(info.buffer_size());
    let (w, h) = (info.width, info.height);
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity((w as usize) * (h as usize) * 4);
            for px in buf.chunks_exact(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity((w as usize) * (h as usize) * 4);
            for &g in &buf {
                out.extend_from_slice(&[g, g, g, 255]);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity((w as usize) * (h as usize) * 4);
            for px in buf.chunks_exact(2) {
                out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
            out
        }
        // normalize_to_color8 之后不应出现调色板
        png::ColorType::Indexed => {
            return Err("PNG 仍为调色板格式（normalize 未生效）".to_string());
        }
    };
    Ok((w, h, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(ext: &str) -> std::path::PathBuf {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(format!("img-roundtrip-{}.{ext}", std::process::id()))
    }

    fn gradient(w: i32, h: i32) -> Frame {
        let mut bgra = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                bgra.extend_from_slice(&[(x * 7) as u8, (y * 11) as u8, ((x + y) * 3) as u8, 255]);
            }
        }
        Frame::new(w, h, bgra)
    }

    #[test]
    fn png_写读往返_颜色一致() {
        let path = tmp_path("png");
        let f = gradient(37, 23);
        write_png(&path, &f).unwrap();
        let (w, h, rgba) = read_image_rgba(&path).unwrap();
        assert_eq!((w as i32, h as i32), (f.width, f.height));
        for i in 0..(f.width * f.height) as usize {
            let px = &f.bgra[i * 4..i * 4 + 4];
            let got = &rgba[i * 4..i * 4 + 4];
            assert_eq!([got[0], got[1], got[2]], [px[2], px[1], px[0]]);
            assert_eq!(got[3], 255);
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn jpeg_写读往返_尺寸正确且近似原色() {
        let path = tmp_path("jpg");
        let f = gradient(64, 48);
        write_jpeg(&path, &f).unwrap();
        let (w, h, rgba) = read_image_rgba(&path).unwrap();
        assert_eq!((w as i32, h as i32), (f.width, f.height));
        // 有损压缩：只校验量级接近（±12）而不是逐像素相等
        let mid = ((24 * f.width + 32) * 4) as usize;
        let px = &f.bgra[mid..mid + 4];
        let got = &rgba[mid..mid + 4];
        assert!((got[0] as i32 - px[2] as i32).abs() <= 12, "R: {} vs {}", got[0], px[2]);
        assert!((got[1] as i32 - px[1] as i32).abs() <= 12, "G: {} vs {}", got[1], px[1]);
        assert!((got[2] as i32 - px[0] as i32).abs() <= 12, "B: {} vs {}", got[2], px[0]);
        let _ = std::fs::remove_file(&path);
    }

}
