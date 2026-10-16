//! Offline spike: in-memory BGRA frames -> H.264 MP4.
//!
//! Encoder: `libopenh264`, statically compiled into
//! `D:\CapLite\ffmpeg-min\bin\avcodec-63.dll`.
//! Container: libavformat `mp4` muxer.
//! Pixel conversion: libswscale BGRA -> YUV420P.
//!
//! Run:  cargo run --offline --release -- out.mp4
//!
//! Every step is asserted and printed; nothing is merely "assumed to work".

mod ffi;
mod mp4;

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;

use ffi::off::*;
use ffi::*;

const W: i32 = 640;
const H: i32 = 360;
const FPS: i32 = 10;
const NFRAMES: i32 = 48;
const BITRATE: i64 = 2_000_000;
const ENCODER_NAME: &str = "libopenh264";
const AV_PKT_FLAG_KEY: c_int = 1;

fn section(title: &str) {
    println!("\n================ {title} ================");
}

fn av_err(e: c_int) -> String {
    let mut buf = [0 as c_char; 256];
    unsafe { av_strerror(e, buf.as_mut_ptr(), buf.len()) };
    unsafe { cstr(buf.as_ptr()) }
}

fn ck(what: &str, r: c_int) -> Result<c_int, String> {
    if r < 0 {
        Err(format!("{what} failed: {r} ({})", av_err(r)))
    } else {
        Ok(r)
    }
}

unsafe fn opt_set_int(obj: *mut c_void, name: &str, v: i64, flags: c_int) {
    let n = CString::new(name).unwrap();
    let r = av_opt_set_int(obj, n.as_ptr(), v, flags);
    println!(
        "  av_opt_set_int(\"{name}\", {v}, flags={flags}) -> {r} {}",
        if r < 0 { "(NOT an option)" } else { "OK" }
    );
}

unsafe fn opt_set_str(obj: *mut c_void, name: &str, v: &str, flags: c_int) {
    let n = CString::new(name).unwrap();
    let s = CString::new(v).unwrap();
    let r = av_opt_set(obj, n.as_ptr(), s.as_ptr(), flags);
    println!(
        "  av_opt_set(\"{name}\" = \"{v}\", flags={flags}) -> {r} {}",
        if r < 0 { "(NOT an option)" } else { "OK" }
    );
}

// ---------------------------------------------------------------------------
// 1. What is actually inside avcodec-63.dll?
// ---------------------------------------------------------------------------

unsafe fn enumerate_codecs() -> Result<(), String> {
    section("1. codecs registered in avcodec-63.dll (av_codec_iterate)");
    let mut opaque: *mut c_void = ptr::null_mut();
    let mut n = 0;
    loop {
        let c = av_codec_iterate(&mut opaque);
        if c.is_null() {
            break;
        }
        n += 1;
        let name = cstr(get_ptr::<c_char>(c, CODEC_NAME));
        let long = cstr(get_ptr::<c_char>(c, CODEC_LONG_NAME));
        let wrap = cstr(get_ptr::<c_char>(c, CODEC_WRAPPER_NAME));
        let ty = get_i32(c, CODEC_TYPE);
        let id = get_i32(c, CODEC_ID);
        let caps = get_i32(c, CODEC_CAPABILITIES);
        let kind = if av_codec_is_encoder(c) != 0 {
            "ENCODER"
        } else {
            "DECODER"
        };
        let tyname = match ty {
            0 => "VIDEO",
            1 => "AUDIO",
            2 => "DATA",
            3 => "SUBTITLE",
            4 => "ATTACHMENT",
            _ => "?",
        };
        println!(
            "  [{kind}] type={tyname} id={id} name={name:<16} wrapper={:<10} caps=0x{caps:x} long_name={long}",
            if wrap.is_empty() { "-" } else { wrap.as_str() }
        );
    }
    println!("  total codecs registered: {n}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Synthetic BGRA frames
// ---------------------------------------------------------------------------

/// Deterministic, guaranteed-different-per-frame BGRA image: a scrolling
/// gradient plus a moving opaque rectangle (real inter-frame motion).
fn synth(buf: &mut [u8], t: i32) {
    for y in 0..H {
        let row = y * W;
        for x in 0..W {
            let o = ((row + x) * 4) as usize;
            buf[o] = ((x + t * 7) & 0xff) as u8; // B
            buf[o + 1] = ((y + t * 5) & 0xff) as u8; // G
            buf[o + 2] = (((x ^ y) + t * 11) & 0xff) as u8; // R
            buf[o + 3] = 255; // A
        }
    }
    let (bw, bh) = (96, 72);
    let bx = (t * 13) % (W - bw);
    let by = (t * 7) % (H - bh);
    for y in by..by + bh {
        let row = y * W;
        for x in bx..bx + bw {
            let o = ((row + x) * 4) as usize;
            buf[o] = 0;
            buf[o + 1] = 0;
            buf[o + 2] = 255; // red block
        }
    }
}

fn fnv1a(px: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &x in px {
        h ^= x as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// BT.601 limited-range luma of a BGRA buffer, used to cross-check the decode.
fn bgra_luma_mean(px: &[u8]) -> f64 {
    let n = (W * H) as usize;
    let mut acc = 0f64;
    for i in 0..n {
        let o = i * 4;
        let (b, g, r) = (px[o] as f64, px[o + 1] as f64, px[o + 2] as f64);
        acc += 16.0 + (0.299 * r + 0.587 * g + 0.114 * b) * (219.0 / 255.0);
    }
    acc / n as f64
}

// ---------------------------------------------------------------------------
// 2. Encode
// ---------------------------------------------------------------------------

unsafe fn drain_packets(
    ctx: *mut AVCodecContext,
    pkt: *mut AVPacket,
    oc: *mut AVFormatContext,
    dst_tb: AVRational,
    total_bytes: &mut u64,
    verbose: bool,
) -> Result<usize, String> {
    let src_tb = get_rat(ctx, CC_TIME_BASE);
    let mut n = 0;
    loop {
        let r = avcodec_receive_packet(ctx, pkt);
        if r == AVERROR_EAGAIN || r == AVERROR_EOF {
            break;
        }
        if r < 0 {
            return Err(format!("avcodec_receive_packet failed: {r} ({})", av_err(r)));
        }
        let size = get_i32(pkt, PKT_SIZE);
        *total_bytes += size as u64;
        set_i32(pkt, PKT_STREAM_INDEX, 0);
        av_packet_rescale_ts(pkt, src_tb, dst_tb);
        let pts = get_i64(pkt, PKT_PTS);
        let dts = get_i64(pkt, PKT_DTS);
        let dur = get_i64(pkt, PKT_DURATION);
        let flags = get_i32(pkt, PKT_FLAGS);
        if verbose {
            println!(
                "    packet: size={size:>6} pts={pts:>5} dts={dts:>5} duration={dur:>4} key={} (rescaled {}/{} -> {}/{})",
                flags & AV_PKT_FLAG_KEY != 0,
                src_tb.num,
                src_tb.den,
                dst_tb.num,
                dst_tb.den
            );
        }
        ck("av_interleaved_write_frame", av_interleaved_write_frame(oc, pkt))?;
        av_packet_unref(pkt);
        n += 1;
    }
    Ok(n)
}

unsafe fn encode(path: &str, verbose: bool) -> Result<(), String> {
    section(&format!(
        "2. encode {NFRAMES} synthetic BGRA frames ({W}x{H} @ {FPS}fps) -> H.264 MP4"
    ));

    let cname = CString::new(ENCODER_NAME).unwrap();
    let codec = avcodec_find_encoder_by_name(cname.as_ptr());
    if codec.is_null() {
        return Err(format!(
            "avcodec_find_encoder_by_name(\"{ENCODER_NAME}\") returned NULL"
        ));
    }
    println!(
        "  encoder found: name={} long_name={} wrapper={}",
        cstr(get_ptr::<c_char>(codec, CODEC_NAME)),
        cstr(get_ptr::<c_char>(codec, CODEC_LONG_NAME)),
        cstr(get_ptr::<c_char>(codec, CODEC_WRAPPER_NAME))
    );

    let ctx = avcodec_alloc_context3(codec);
    if ctx.is_null() {
        return Err("avcodec_alloc_context3 returned NULL".into());
    }

    // --- fields with NO AVOption, written at measured offsets (see ffi.rs) ---
    set_i32(ctx, CC_WIDTH, W);
    set_i32(ctx, CC_HEIGHT, H);
    set_i32(ctx, CC_PIX_FMT, AV_PIX_FMT_YUV420P);
    set_rat(ctx, CC_TIME_BASE, AVRational::new(1, FPS));
    set_rat(ctx, CC_FRAMERATE, AVRational::new(FPS, 1));
    set_rat(ctx, CC_SAMPLE_ASPECT_RATIO, AVRational::new(1, 1));
    set_i32(ctx, CC_GOP_SIZE, FPS);
    set_i32(ctx, CC_MAX_B_FRAMES, 0);
    set_i64(ctx, CC_BIT_RATE, BITRATE);
    set_i32(ctx, CC_RC_MAX_RATE, BITRATE as c_int);
    set_i32(ctx, CC_RC_BUFFER_SIZE, (BITRATE / 2) as c_int);
    set_i32(ctx, CC_THREAD_COUNT, 1);
    let flags = get_i32(ctx, CC_FLAGS);
    set_i32(ctx, CC_FLAGS, flags | AV_CODEC_FLAG_GLOBAL_HEADER);

    println!("  fields written directly at measured offsets:");
    println!(
        "    width={} height={} pix_fmt={} time_base={}/{} framerate={}/{} gop_size={} max_b_frames={}",
        get_i32(ctx, CC_WIDTH),
        get_i32(ctx, CC_HEIGHT),
        get_i32(ctx, CC_PIX_FMT),
        get_rat(ctx, CC_TIME_BASE).num,
        get_rat(ctx, CC_TIME_BASE).den,
        get_rat(ctx, CC_FRAMERATE).num,
        get_rat(ctx, CC_FRAMERATE).den,
        get_i32(ctx, CC_GOP_SIZE),
        get_i32(ctx, CC_MAX_B_FRAMES),
    );
    println!(
        "    bit_rate={} flags=0x{:x} (GLOBAL_HEADER set = {})",
        get_i64(ctx, CC_BIT_RATE),
        get_i32(ctx, CC_FLAGS),
        get_i32(ctx, CC_FLAGS) & AV_CODEC_FLAG_GLOBAL_HEADER != 0
    );

    println!("  AVOptions (these DO exist on AVCodecContext):");
    opt_set_int(ctx, "b", BITRATE, 0);
    opt_set_int(ctx, "g", FPS as i64, 0);
    opt_set_int(ctx, "bf", 0, 0);
    opt_set_int(ctx, "threads", 1, 0);
    println!("  libopenh264 wrapper private options (set on ctx->priv_data, offset {CC_PRIV_DATA}):");
    // NOTE: av_opt_set(ctx, "rc_mode", ..., AV_OPT_SEARCH_CHILDREN) returns
    // AVERROR_OPTION_NOT_FOUND because AVCodecContext's AVClass does not expose
    // the wrapper's priv_class through child iteration here.  Setting it on
    // ctx->priv_data directly works (measured offset 32).
    let priv_data = get_ptr::<c_void>(ctx, CC_PRIV_DATA);
    println!("  ctx->priv_data = {priv_data:p}");
    let profile = std::env::var("SPIKE_PROFILE").unwrap_or_else(|_| "constrained_baseline".into());
    println!("  SPIKE_PROFILE = {profile}");
    opt_set_str(priv_data, "profile", &profile, 0);
    opt_set_str(priv_data, "rc_mode", "bitrate", 0);
    opt_set_int(priv_data, "allow_skip_frames", 0, 0);

    ck("avcodec_open2", avcodec_open2(ctx, codec, ptr::null_mut()))?;
    println!("  avcodec_open2 OK");
    println!(
        "  context after open: width={} height={} pix_fmt={} sw_pix_fmt={} time_base={}/{} extradata_size={}",
        get_i32(ctx, CC_WIDTH),
        get_i32(ctx, CC_HEIGHT),
        get_i32(ctx, CC_PIX_FMT),
        get_i32(ctx, CC_SW_PIX_FMT),
        get_rat(ctx, CC_TIME_BASE).num,
        get_rat(ctx, CC_TIME_BASE).den,
        get_i32(ctx, CC_EXTRA_DATA_SIZE)
    );

    // --- swscale BGRA -> YUV420P ---
    let sws = sws_getContext(
        W,
        H,
        AV_PIX_FMT_BGRA,
        W,
        H,
        AV_PIX_FMT_YUV420P,
        SWS_BILINEAR,
        ptr::null_mut(),
        ptr::null_mut(),
        ptr::null(),
    );
    if sws.is_null() {
        return Err("sws_getContext(BGRA->YUV420P) returned NULL".into());
    }
    println!("  sws_getContext(BGRA({AV_PIX_FMT_BGRA}) -> YUV420P({AV_PIX_FMT_YUV420P})) OK");

    // --- reusable encoder-side frame ---
    let mut frame = av_frame_alloc();
    if frame.is_null() {
        return Err("av_frame_alloc returned NULL".into());
    }
    set_i32(frame, FRAME_FORMAT, AV_PIX_FMT_YUV420P);
    set_i32(frame, FRAME_WIDTH, W);
    set_i32(frame, FRAME_HEIGHT, H);
    ck("av_frame_get_buffer", av_frame_get_buffer(frame, 32))?;
    println!(
        "  av_frame_get_buffer OK: Y linesize={} U linesize={} V linesize={}",
        arr_i32(frame, FRAME_LINESIZE, 0),
        arr_i32(frame, FRAME_LINESIZE, 1),
        arr_i32(frame, FRAME_LINESIZE, 2)
    );

    // --- output (muxer) side ---
    let fmt = CString::new("mp4").unwrap();
    let fname = CString::new(path).unwrap();
    let mut oc: *mut AVFormatContext = ptr::null_mut();
    ck(
        "avformat_alloc_output_context2",
        avformat_alloc_output_context2(&mut oc, ptr::null(), fmt.as_ptr(), fname.as_ptr()),
    )?;

    let st = avformat_new_stream(oc, ptr::null());
    if st.is_null() {
        return Err("avformat_new_stream returned NULL".into());
    }
    let codecpar = get_ptr::<AVCodecParameters>(st, ST_CODECPAR);
    ck(
        "avcodec_parameters_from_context",
        avcodec_parameters_from_context(codecpar, ctx),
    )?;
    println!(
        "  stream codecpar: codec_type={} codec_id={} width={} height={} format={} bit_rate={} extradata_size={}",
        get_i32(codecpar, CP_CODEC_TYPE),
        get_i32(codecpar, CP_CODEC_ID),
        get_i32(codecpar, CP_WIDTH),
        get_i32(codecpar, CP_HEIGHT),
        get_i32(codecpar, CP_FORMAT),
        get_i64(codecpar, CP_BIT_RATE),
        get_i32(codecpar, CP_EXTRA_DATA_SIZE),
    );

    let dst_tb = AVRational::new(1, 1000);
    set_rat(st, ST_TIME_BASE, dst_tb);
    set_rat(st, ST_AVG_FRAME_RATE, AVRational::new(FPS, 1));
    set_rat(st, ST_R_FRAME_RATE, AVRational::new(FPS, 1));

    let pb_slot = (oc as *mut u8).add(FC_PB) as *mut *mut AVIOContext;
    ck(
        "avio_open",
        avio_open(pb_slot, fname.as_ptr(), AVIO_FLAG_WRITE),
    )?;
    ck(
        "avformat_write_header",
        avformat_write_header(oc, ptr::null_mut()),
    )?;
    // IMPORTANT: the mov/mp4 muxer OVERRIDES st->time_base during
    // avformat_write_header (measured: 1/1000 requested -> 1/16000 actual).
    // Packet timestamps must therefore be rescaled to the value it chose, read
    // back *after* write_header.  Rescaling to the requested 1/1000 instead
    // makes the file 16x too short.
    let dst_tb = get_rat(st, ST_TIME_BASE);
    println!(
        "  avformat_write_header OK: requested st.time_base=1/1000, muxer chose {}/{}",
        dst_tb.num, dst_tb.den
    );

    // --- encode loop ---
    let mut src = vec![0u8; (W * H * 4) as usize];
    let mut pkt = av_packet_alloc();
    if pkt.is_null() {
        return Err("av_packet_alloc returned NULL".into());
    }
    let mut total_bytes: u64 = 0;
    let mut packets = 0usize;
    let mut hashes = Vec::with_capacity(NFRAMES as usize);
    let mut src_luma = Vec::with_capacity(NFRAMES as usize);

    for i in 0..NFRAMES {
        synth(&mut src, i);
        hashes.push(fnv1a(&src));
        src_luma.push(bgra_luma_mean(&src));

        ck("av_frame_make_writable", av_frame_make_writable(frame))?;

        let sp: [*const u8; 4] = [src.as_ptr(), ptr::null(), ptr::null(), ptr::null()];
        let ss: [c_int; 4] = [W * 4, 0, 0, 0];
        let dp: [*mut u8; 4] = [
            arr_ptr(frame, FRAME_DATA, 0),
            arr_ptr(frame, FRAME_DATA, 1),
            arr_ptr(frame, FRAME_DATA, 2),
            arr_ptr(frame, FRAME_DATA, 3),
        ];
        let ds: [c_int; 4] = [
            arr_i32(frame, FRAME_LINESIZE, 0),
            arr_i32(frame, FRAME_LINESIZE, 1),
            arr_i32(frame, FRAME_LINESIZE, 2),
            arr_i32(frame, FRAME_LINESIZE, 3),
        ];
        let rows = sws_scale(sws, sp.as_ptr(), ss.as_ptr(), 0, H, dp.as_ptr(), ds.as_ptr());
        if rows != H {
            return Err(format!("sws_scale returned {rows} rows, expected {H}"));
        }

        set_i64(frame, FRAME_PTS, i as i64);
        if verbose {
            println!(
                "  frame {i:>2}: bgra fnv1a=0x{:016x} luma_mean={:.3}",
                hashes[i as usize], src_luma[i as usize]
            );
        }
        ck(
            &format!("avcodec_send_frame(#{i})"),
            avcodec_send_frame(ctx, frame),
        )?;
        packets += drain_packets(ctx, pkt, oc, dst_tb, &mut total_bytes, verbose)?;
    }

    // flush
    ck(
        "avcodec_send_frame(NULL) [flush]",
        avcodec_send_frame(ctx, ptr::null()),
    )?;
    packets += drain_packets(ctx, pkt, oc, dst_tb, &mut total_bytes, verbose)?;

    ck("av_write_trailer", av_write_trailer(oc))?;
    avio_closep(pb_slot);

    let distinct = {
        let mut v = hashes.clone();
        v.sort_unstable();
        v.dedup();
        v.len()
    };
    println!("  frames submitted            : {NFRAMES}");
    println!("  distinct source frame hashes: {distinct}");
    println!("  H.264 access units written  : {packets}");
    println!("  total elementary-stream data: {total_bytes} bytes");

    // --- cleanup ---
    av_packet_free(&mut pkt);
    av_frame_free(&mut frame);
    sws_freeContext(sws);
    let mut ctx_mut = ctx;
    avcodec_free_context(&mut ctx_mut);
    avformat_free_context(oc);

    if distinct != NFRAMES as usize {
        return Err(format!(
            "source frames were not all distinct ({distinct}/{NFRAMES})"
        ));
    }
    if packets == 0 || total_bytes == 0 {
        return Err("encoder produced no output".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 3. MP4 box structure
// ---------------------------------------------------------------------------

fn verify_mp4(path: &str) -> Result<mp4::Mp4Info, String> {
    section("3. MP4 box structure of the produced file");
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    println!("  file size: {} bytes", bytes.len());
    let info = mp4::parse(&bytes)?;
    println!("  top-level boxes: {}", info.top_level_boxes.join(", "));
    println!(
        "  ftyp={} major_brand={} compatible={:?}",
        info.has_ftyp, info.major_brand, info.compatible_brands
    );
    println!(
        "  moov={} mdat={} mdat_bytes={} mdat_boxes={}",
        info.has_moov, info.has_mdat, info.mdat_bytes, info.mdat_count
    );
    println!(
        "  mvhd timescale={} duration={} => {:.3} s",
        info.timescale,
        info.duration,
        info.duration_secs()
    );
    println!("  streams found: {}", info.streams.len());
    for (i, s) in info.streams.iter().enumerate() {
        println!(
            "    trak[{i}]: handler={} fourcc={} {}x{} timescale={} duration={} ({:.3} s) samples={} sample_bytes={} avg_fps={:.3} sync_samples={:?} chunks={:?}",
            s.handler,
            s.codec_fourcc,
            s.width,
            s.height,
            s.timescale,
            s.duration,
            s.duration_secs(),
            s.sample_count,
            s.sample_bytes,
            s.avg_fps(),
            s.sync_samples,
            s.chunk_count
        );
    }
    println!("  --- full box tree ---");
    for l in &info.log {
        println!("  {l}");
    }

    if !(info.has_ftyp && info.has_moov && info.has_mdat) {
        return Err("missing ftyp/moov/mdat".into());
    }
    if info.mdat_bytes == 0 {
        return Err("mdat is empty".into());
    }
    let v = info.video().ok_or("no video track found")?;
    if v.sample_count != NFRAMES as u32 {
        return Err(format!(
            "video track has {} samples, expected {NFRAMES}",
            v.sample_count
        ));
    }
    if v.width as i32 != W || v.height as i32 != H {
        return Err(format!(
            "video track is {}x{}, expected {W}x{H}",
            v.width, v.height
        ));
    }
    let d = v.duration_secs();
    if (d - NFRAMES as f64 / FPS as f64).abs() > 0.2 {
        return Err(format!("video track duration {d:.3}s is not ~4.8s"));
    }
    Ok(info)
}

// ---------------------------------------------------------------------------
// 4. Real decode-back
// ---------------------------------------------------------------------------

unsafe fn verify_decode(path: &str) -> Result<(i32, i32, i32, Vec<f64>), String> {
    section("4. real decode-back: ffmpeg-min mov demuxer + ffmpeg-min h264 decoder");
    let cpath = CString::new(path).unwrap();
    let mut ic: *mut AVFormatContext = ptr::null_mut();
    ck(
        "avformat_open_input",
        avformat_open_input(&mut ic, cpath.as_ptr(), ptr::null(), ptr::null_mut()),
    )?;
    ck(
        "avformat_find_stream_info",
        avformat_find_stream_info(ic, ptr::null_mut()),
    )?;

    let nb = get_i32(ic, FC_NB_STREAMS);
    println!("  demuxed nb_streams={nb}");
    let streams_base = get_ptr::<c_void>(ic, FC_STREAMS);
    println!("  ic->streams (AVStream **) = {streams_base:p}");
    if streams_base.is_null() {
        return Err("AVFormatContext.streams is NULL".into());
    }
    for i in 0..nb as usize {
        let st = *((streams_base as *const *mut AVStream).add(i));
        println!("    stream[{i}] AVStream* = {st:p}");
        if st.is_null() {
            return Err(format!("stream[{i}] is NULL"));
        }
        let cp = get_ptr::<AVCodecParameters>(st, ST_CODECPAR);
        println!("    stream[{i}] codecpar = {cp:p}");
        if cp.is_null() {
            return Err(format!("stream[{i}] codecpar is NULL"));
        }
        println!(
            "    stream[{i}] codec_id={} codec_type={} {}x{} pix_fmt={} extradata_size={} time_base={}/{}",
            get_i32(cp, CP_CODEC_ID),
            get_i32(cp, CP_CODEC_TYPE),
            get_i32(cp, CP_WIDTH),
            get_i32(cp, CP_HEIGHT),
            get_i32(cp, CP_FORMAT),
            get_i32(cp, CP_EXTRA_DATA_SIZE),
            get_rat(st, ST_TIME_BASE).num,
            get_rat(st, ST_TIME_BASE).den
        );
    }

    let mut dec: *const AVCodec = ptr::null();
    let sidx = av_find_best_stream(ic, AVMEDIA_TYPE_VIDEO, -1, -1, &mut dec, 0);
    ck("av_find_best_stream(AVMEDIA_TYPE_VIDEO)", sidx)?;
    if dec.is_null() {
        let n = CString::new("h264").unwrap();
        dec = avcodec_find_decoder_by_name(n.as_ptr());
        println!("  (av_find_best_stream gave no decoder, used avcodec_find_decoder_by_name)");
    } else {
        println!(
            "  av_find_best_stream picked decoder: {}",
            cstr(get_ptr::<c_char>(dec, CODEC_NAME))
        );
    }
    if dec.is_null() {
        return Err("no H.264 decoder available in this build".into());
    }

    // NOTE: AVFormatContext.streams is a `AVStream **` (pointer to an array),
    // NOT an inline array, so the array element must be dereferenced twice.
    let st = *((streams_base as *const *mut AVStream).add(sidx as usize));
    println!("  chosen stream[{sidx}] AVStream* = {st:p}");
    if st.is_null() {
        return Err("chosen AVStream is NULL".into());
    }
    let cp = get_ptr::<AVCodecParameters>(st, ST_CODECPAR);
    println!("  chosen stream codecpar = {cp:p}");
    if cp.is_null() {
        return Err("chosen AVCodecParameters is NULL".into());
    }
    let mut dctx = avcodec_alloc_context3(dec);
    println!("  avcodec_alloc_context3(h264) = {dctx:p}");
    if dctx.is_null() {
        return Err("avcodec_alloc_context3(decoder) returned NULL".into());
    }
    ck(
        "avcodec_parameters_to_context",
        avcodec_parameters_to_context(dctx, cp),
    )?;
    println!("  avcodec_parameters_to_context OK");
    ck(
        "avcodec_open2(decoder)",
        avcodec_open2(dctx, dec, ptr::null_mut()),
    )?;
    println!("  avcodec_open2(decoder) OK");
    println!(
        "  decoder open: width={} height={} pix_fmt={}",
        get_i32(dctx, CC_WIDTH),
        get_i32(dctx, CC_HEIGHT),
        get_i32(dctx, CC_PIX_FMT)
    );

    let mut dframe = av_frame_alloc();
    let mut dpkt = av_packet_alloc();
    let mut frames = 0usize;
    let mut dims: Option<(i32, i32, i32)> = None;
    let mut luma: Vec<f64> = Vec::new();
    let mut pkt_count = 0usize;

    macro_rules! pull {
        () => {
            loop {
                let rr = avcodec_receive_frame(dctx, dframe);
                if rr < 0 {
                    break;
                }
                frames += 1;
                let (fw, fh, ff) = (
                    get_i32(dframe, FRAME_WIDTH),
                    get_i32(dframe, FRAME_HEIGHT),
                    get_i32(dframe, FRAME_FORMAT),
                );
                if dims.is_none() {
                    dims = Some((fw, fh, ff));
                    println!(
                        "  first decoded frame: {}x{} format={} linesize[0]={}",
                        fw,
                        fh,
                        ff,
                        arr_i32(dframe, FRAME_LINESIZE, 0)
                    );
                }
                let yp = arr_ptr::<u8>(dframe, FRAME_DATA, 0);
                let stride = arr_i32(dframe, FRAME_LINESIZE, 0) as isize;
                let mut acc = 0f64;
                for y in 0..fh as isize {
                    let row = yp.offset(y * stride);
                    for x in 0..fw as isize {
                        acc += *row.offset(x) as f64;
                    }
                }
                luma.push(acc / (fw as f64 * fh as f64));
                av_frame_unref(dframe);
            }
        };
    }

    loop {
        let r = av_read_frame(ic, dpkt);
        if r < 0 {
            break;
        }
        pkt_count += 1;
        if get_i32(dpkt, PKT_STREAM_INDEX) == sidx {
            let s = avcodec_send_packet(dctx, dpkt);
            if s < 0 {
                return Err(format!("avcodec_send_packet failed: {s} ({})", av_err(s)));
            }
            pull!();
        }
        av_packet_unref(dpkt);
    }
    avcodec_send_packet(dctx, ptr::null());
    pull!();

    println!("  demuxed packets: {pkt_count}");
    println!("  DECODED FRAMES : {frames}");
    let (fw, fh, ff) = dims.ok_or("decoder produced no frames")?;
    println!("  decoded size   : {fw}x{fh} pix_fmt={ff}");
    let lo = luma.iter().cloned().fold(f64::INFINITY, f64::min);
    let hi = luma.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    println!(
        "  decoded Y mean : min={lo:.3} max={hi:.3} spread={:.3}",
        hi - lo
    );

    av_packet_free(&mut dpkt);
    av_frame_free(&mut dframe);
    avcodec_free_context(&mut dctx);
    avformat_close_input(&mut ic);

    if frames != NFRAMES as usize {
        return Err(format!("decoded {frames} frames, expected {NFRAMES}"));
    }
    if fw != W || fh != H {
        return Err(format!("decoded {fw}x{fh}, expected {W}x{H}"));
    }
    Ok((fw, fh, ff, luma))
}

// ---------------------------------------------------------------------------

fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let ma = a[..n].iter().sum::<f64>() / n as f64;
    let mb = b[..n].iter().sum::<f64>() / n as f64;
    let mut num = 0.0;
    let mut da = 0.0;
    let mut db = 0.0;
    for i in 0..n {
        let x = a[i] - ma;
        let y = b[i] - mb;
        num += x * y;
        da += x * x;
        db += y * y;
    }
    if da == 0.0 || db == 0.0 {
        0.0
    } else {
        num / (da.sqrt() * db.sqrt())
    }
}

fn run() -> Result<(), String> {
    unsafe {
        av_log_set_level(AV_LOG_INFO);
    }

    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "out.mp4".to_string());
    let verbose = std::env::var("SPIKE_VERBOSE")
        .map(|v| v != "0")
        .unwrap_or(true);

    section("0. identity");
    println!("  rust target   : {}", std::env::consts::ARCH);
    println!(
        "  ffmpeg-min dir: {}",
        std::env::var("FFMPEG_MIN_DIR").unwrap_or_else(|_| r"D:\CapLite\ffmpeg-min".into())
    );
    println!("  output file   : {path}");
    unsafe {
        println!(
            "  avcodec_configuration():\n    {}",
            cstr(ffi::avcodec_configuration())
        );
    }

    unsafe {
        enumerate_codecs()?;
        encode(&path, verbose)?;
    }
    let info = verify_mp4(&path)?;
    let (fw, fh, fmt, dec_luma) = unsafe { verify_decode(&path)? };

    section("5. verdict");
    println!("  encoder       : libopenh264 (statically inside avcodec-63.dll)");
    println!("  mp4 muxer     : libavformat 'mp4'");
    println!("  file          : {path}");
    println!(
        "  mvhd duration : {:.3} s @ timescale {}",
        info.duration_secs(),
        info.timescale
    );
    if let Some(v) = info.video() {
        println!(
            "  video track   : {} {}x{} samples={} sync={:?} bytes={}",
            v.codec_fourcc, v.width, v.height, v.sample_count, v.sync_samples, v.sample_bytes
        );
    }
    println!(
        "  decoded back  : {fw}x{fh} pix_fmt={fmt} frames={}",
        dec_luma.len()
    );

    let n = NFRAMES as usize;
    let mut src_luma = Vec::with_capacity(n);
    {
        let mut buf = vec![0u8; (W * H * 4) as usize];
        for i in 0..NFRAMES {
            synth(&mut buf, i);
            src_luma.push(bgra_luma_mean(&buf));
        }
    }
    let max_dev = (0..n.min(dec_luma.len()))
        .map(|i| (dec_luma[i] - src_luma[i]).abs())
        .fold(0f64, f64::max);
    let corr = pearson(&src_luma, &dec_luma);
    println!(
        "  source BGRA luma means : {:?}",
        src_luma
            .iter()
            .map(|v| (v * 10.0).round() / 10.0)
            .collect::<Vec<_>>()
    );
    println!(
        "  decoded Y means        : {:?}",
        dec_luma
            .iter()
            .map(|v| (v * 10.0).round() / 10.0)
            .collect::<Vec<_>>()
    );
    println!("  max |decoded-expected| = {max_dev:.3}   pearson r = {corr:.5}");

    if dec_luma.len() != n {
        return Err(format!("decoded {} frames, expected {n}", dec_luma.len()));
    }
    if corr < 0.98 {
        return Err(format!(
            "decoded luma does not track the source (pearson r={corr:.5})"
        ));
    }
    println!("\n  *** SPIKE PASSED: BGRA frames -> H.264 MP4 -> decoded back consistently ***");
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("\n!!! SPIKE FAILED: {e}");
        std::process::exit(1);
    }
}
