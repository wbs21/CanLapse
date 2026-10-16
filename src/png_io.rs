//! PNG 读写。
//!
//! 用 `png` crate 而不是自己搓 —— 帧缓存动辄几万张 PNG，压缩率直接影响磁盘占用。

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use crate::frame::Frame;

pub fn write_png(path: &Path, frame: &Frame) -> std::io::Result<()> {
    let rgba = frame.to_rgba();
    let file = File::create(path)?;
    let w = BufWriter::new(file);
    let mut enc = png::Encoder::new(w, frame.width as u32, frame.height as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header()?;
    writer.write_image_data(&rgba)?;
    writer.finish()?;
    Ok(())
}

/// 读回 PNG 为 RGBA（重新导出时要用；OpenH264 需要的是 YUV420，转换在编码模块里做）
pub fn read_png_rgba(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    let file = File::open(path).map_err(|e| format!("打开 {} 失败: {e}", path.display()))?;
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder.read_info().map_err(|e| format!("解析 PNG 失败: {e}"))?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| "无法确定 PNG 输出缓冲大小".to_string())?;
    let mut buf = vec![0u8; size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("读取 PNG 帧失败: {e}"))?;
    buf.truncate(info.buffer_size());
    Ok((info.width, info.height, buf))
}
