//! PNG 读写。
//!
//! 用 `png` crate 而不是自己搓 —— 帧缓存动辄几万张 PNG，压缩率直接影响磁盘占用。
//!
//! 写盘用 **RGB**（不是 RGBA）：帧本来就不透明，alpha 通道只会让文件更大、编码更慢。
//! 实测 1920×1009 画作：RGBA 1039 KB / ~90 ms → RGB 942 KB / ~68 ms。
//! 读回时兼容两种（旧会话里是 RGBA），统一转成 RGBA 给编码器。

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use crate::frame::Frame;

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

/// 读回 PNG 为 RGBA（重新导出时要用；OpenH264 需要的是 YUV420，转换在编码模块里做）
pub fn read_png_rgba(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    let file = File::open(path).map_err(|e| format!("打开 {} 失败: {e}", path.display()))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    // 调色板 / 低位深统一展开成 8 位；RGB 仍保持 RGB，下面再补 alpha
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

    #[test]
    fn png_写读往返_颜色一致() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("png-roundtrip-{}.png", std::process::id()));
        // 造一张有渐变和纯色块的帧（覆盖压缩/过滤分支）
        let (w, h) = (37, 23);
        let mut bgra = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                bgra.extend_from_slice(&[
                    (x * 7) as u8,
                    (y * 11) as u8,
                    ((x + y) * 3) as u8,
                    255,
                ]);
            }
        }
        let f = Frame::new(w, h, bgra.clone());
        write_png(&path, &f).unwrap();
        let (rw, rh, rgba) = read_png_rgba(&path).unwrap();
        assert_eq!((rw as i32, rh as i32), (w, h));
        for i in 0..(w * h) as usize {
            let px = &bgra[i * 4..i * 4 + 4];
            let got = &rgba[i * 4..i * 4 + 4];
            assert_eq!([got[0], got[1], got[2]], [px[2], px[1], px[0]]);
            assert_eq!(got[3], 255);
        }
        let _ = std::fs::remove_file(&path);
    }
}
