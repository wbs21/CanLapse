//! 端到端集成测试：Session → 帧文件 → encode_session → MP4 输出验证。
//!
//! 这些测试只做"管道通不通"的校验（文件存在、大小合理、帧数对得上），
//! 不解析 MP4 容器内部结构 —— 那是 ffmpeg 自己保证的事。
//!
//! 如果本文件跑不起来（缺 FFmpeg 静态库 / OpenH264 编码器不可用），
//! `encoder_available()` 会返回 false，测试会自动 skip。

use std::path::PathBuf;

fn make_tmp_dir(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("canlapse_inttest_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn cleanup(dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// 如果编码器不可用，返回一个会让测试自动 skip 的哨兵。
fn ensure_encoder() -> bool {
    canlapse::encode::encoder_available()
}

#[test]
fn encode_纯静态帧产出可播放的mp4() {
    if !ensure_encoder() {
        eprintln!("[skip] OpenH264 编码器不可用");
        return;
    }

    // 1. 准备帧目录：3 帧 32×32 纯色 BGRA → PNG
    let frames_dir = make_tmp_dir("static");
    let output = frames_dir.join("out.mp4");

    for i in 0..3u32 {
        let mut bgra = vec![0u8; 32 * 32 * 4];
        for px in bgra.chunks_exact_mut(4) {
            // 每帧颜色略变，保证编码器有内容可压
            px[0] = (i * 80) as u8; // B
            px[1] = 128; // G
            px[2] = 64; // R
            px[3] = 255; // A
        }
        let frame = canlapse::frame::Frame::new(32, 32, bgra);
        let png_path = frames_dir.join(format!("frame_{i:04}.png"));
        canlapse::img_io::write_png(&png_path, &frame).unwrap();
    }

    // 2. 调 encode_session
    let opts = canlapse::encode::EncodeOptions {
        fps: 5.0,
        quality: canlapse::encode::Quality::Medium,
        output: output.clone(),
    };
    let mut progress = |_done: u32, _total: u32| canlapse::encode::Progress::Continue;
    let result = canlapse::encode::encode_session(&frames_dir, &opts, &mut progress);

    // 3. 验证
    match result {
        Ok(canlapse::encode::EncodeOutcome::Done(stats)) => {
            assert_eq!(stats.frames, 3, "应编码 3 帧");
            assert_eq!(stats.width, 32, "宽应为 32");
            assert_eq!(stats.height, 32, "高应为 32");
            assert!(stats.bytes > 0, "MP4 文件应非空");
        }
        other => panic!("预期 Done，得到 {other:?}"),
    }

    assert!(output.is_file(), "MP4 应该被创建");
    let meta = std::fs::metadata(&output).unwrap();
    assert!(
        meta.len() > 100,
        "MP4 文件至少有几百字节（有 ftyp/moov/mdat）"
    );

    cleanup(&frames_dir);
}

#[test]
fn encode_空目录返回错误而非panic() {
    if !ensure_encoder() {
        return;
    }
    let dir = make_tmp_dir("empty");
    let opts = canlapse::encode::EncodeOptions {
        fps: 5.0,
        quality: canlapse::encode::Quality::Medium,
        output: dir.join("out.mp4"),
    };
    let mut progress = |_, _| canlapse::encode::Progress::Continue;
    let result = canlapse::encode::encode_session(&dir, &opts, &mut progress);
    assert!(result.is_err(), "空目录应返回错误，不能 panic");
    cleanup(&dir);
}

#[test]
fn encode_fps越界返回错误() {
    if !ensure_encoder() {
        return;
    }
    let dir = make_tmp_dir("fps");
    // 先写一帧让目录有内容（不是空目录错误）
    let mut bgra = vec![0u8; 16 * 16 * 4];
    for px in bgra.chunks_exact_mut(4) {
        px[3] = 255;
    }
    let frame = canlapse::frame::Frame::new(16, 16, bgra);
    canlapse::img_io::write_png(&dir.join("frame_0000.png"), &frame).unwrap();

    let opts = canlapse::encode::EncodeOptions {
        fps: 999.0, // 越界
        quality: canlapse::encode::Quality::Medium,
        output: dir.join("out.mp4"),
    };
    let mut progress = |_, _| canlapse::encode::Progress::Continue;
    let result = canlapse::encode::encode_session(&dir, &opts, &mut progress);
    assert!(result.is_err(), "不合理的 fps 应返回错误");

    let opts_bad = canlapse::encode::EncodeOptions {
        fps: 0.0, // 越界
        quality: canlapse::encode::Quality::Medium,
        output: dir.join("out.mp4"),
    };
    let result2 = canlapse::encode::encode_session(&dir, &opts_bad, &mut progress);
    assert!(result2.is_err());

    cleanup(&dir);
}

#[test]
fn encode_中途取消产出部分视频() {
    if !ensure_encoder() {
        return;
    }
    let dir = make_tmp_dir("cancel");
    let output = dir.join("out.mp4");

    // 写 10 帧
    for i in 0..10u32 {
        let mut bgra = vec![0u8; 32 * 32 * 4];
        for px in bgra.chunks_exact_mut(4) {
            px[0] = (i * 25) as u8;
            px[3] = 255;
        }
        let frame = canlapse::frame::Frame::new(32, 32, bgra);
        canlapse::img_io::write_png(&dir.join(format!("frame_{i:04}.png")), &frame).unwrap();
    }

    let opts = canlapse::encode::EncodeOptions {
        fps: 10.0,
        quality: canlapse::encode::Quality::Medium,
        output: output.clone(),
    };
    let mut call_count = 0u32;
    let mut progress = |_done: u32, _total: u32| {
        call_count += 1;
        if call_count >= 3 {
            canlapse::encode::Progress::Cancel
        } else {
            canlapse::encode::Progress::Continue
        }
    };
    let result = canlapse::encode::encode_session(&dir, &opts, &mut progress);

    // 取消应该是正常返回 Cancelled，且文件已提交（不是 .part）
    match result {
        Ok(canlapse::encode::EncodeOutcome::Cancelled(stats)) => {
            assert!(stats.frames > 0, "取消时至少已编码 1 帧");
        }
        Ok(other) => panic!("预期 Cancelled，得到 {other:?}"),
        Err(e) => panic!("取消不应返回错误：{e}"),
    }
    assert!(output.is_file(), "取消后 MP4 应已提交");

    cleanup(&dir);
}

#[test]
fn 流式编码后仅重封装为普通mp4() {
    if !ensure_encoder() {
        eprintln!("[skip] OpenH264 编码器不可用");
        return;
    }
    let dir = make_tmp_dir("stream_remux");
    let stream = dir.join("stream.mp4");
    let output = dir.join("normal.mp4");
    let mut encoder =
        canlapse::encode::StreamEncoder::new(&stream, 32, 32, canlapse::encode::Quality::Medium)
            .unwrap();
    for i in 0..10u32 {
        let mut bgra = vec![0u8; 32 * 32 * 4];
        for px in bgra.chunks_exact_mut(4) {
            px[0] = (i * 20) as u8;
            px[1] = (255 - i * 20) as u8;
            px[2] = 64;
            px[3] = 255;
        }
        encoder
            .write_frame(&canlapse::frame::Frame::new(32, 32, bgra))
            .unwrap();
    }
    encoder.finish().unwrap();

    assert!(stream.is_file());
    let packets = dir.join("stream.packets");
    assert!(packets.is_file());
    assert_eq!(canlapse::encode::count_stream_frames(&packets).unwrap(), 10);
    assert!(
        !std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with("frame_")),
        "流式录制不应生成图片帧"
    );
    let stream_text = std::fs::read(&stream).unwrap();
    assert!(stream_text.windows(4).any(|w| w == b"moof"));
    assert!(stream_text.windows(4).any(|w| w == b"mvex"));

    let opts = canlapse::encode::EncodeOptions {
        fps: 30.0,
        quality: canlapse::encode::Quality::Medium,
        output: output.clone(),
    };
    let mut progress = |_done: u32, _total: u32| canlapse::encode::Progress::Continue;
    let result = canlapse::encode::encode_session(&dir, &opts, &mut progress);
    match result {
        Ok(canlapse::encode::EncodeOutcome::Done(stats)) => {
            assert_eq!(stats.frames, 10);
            assert_eq!((stats.width, stats.height), (32, 32));
        }
        other => panic!("预期流式重封装成功，得到 {other:?}"),
    }
    let normal = std::fs::read(&output).unwrap();
    assert!(normal.windows(4).any(|w| w == b"moov"));
    assert!(!normal.windows(4).any(|w| w == b"moof"));
    cleanup(&dir);
}
