//! Minimal but real ISO-BMFF (MP4) box parser used to verify `out.mp4`.
//!
//! It walks the top-level box structure, recurses into container boxes, and
//! extracts `ftyp` brand, `mvhd`/`mdhd` timescale+duration, `hdlr` handler
//! types, `stsz` sample counts, `stss` sync-sample counts, `stco` chunk counts,
//! `stsd`/`avc1` width+height and the `avcC` decoder configuration record.
//! No third-party crate is used.

#[derive(Debug, Clone, Default)]
pub struct StreamInfo {
    pub handler: String,
    pub timescale: u32,
    pub duration: u64,
    pub sample_count: u32,
    pub sample_bytes: u64,
    pub sync_samples: Option<u32>,
    pub chunk_count: Option<u32>,
    pub codec_fourcc: String,
    pub width: u16,
    pub height: u16,
    pub avcc: Option<Vec<u8>>,
}

impl StreamInfo {
    pub fn duration_secs(&self) -> f64 {
        if self.timescale == 0 {
            0.0
        } else {
            self.duration as f64 / self.timescale as f64
        }
    }
    pub fn avg_fps(&self) -> f64 {
        let d = self.duration_secs();
        if d > 0.0 {
            self.sample_count as f64 / d
        } else {
            0.0
        }
    }
}

#[derive(Debug, Default)]
pub struct Mp4Info {
    pub top_level_boxes: Vec<String>,
    pub other_top_level_boxes: Vec<(String, u64)>,
    pub major_brand: String,
    pub compatible_brands: Vec<String>,
    pub timescale: u32,
    pub duration: u64,
    pub mdat_bytes: u64,
    pub mdat_count: usize,
    pub has_ftyp: bool,
    pub has_moov: bool,
    pub has_mdat: bool,
    pub streams: Vec<StreamInfo>,
    pub log: Vec<String>,
}

impl Mp4Info {
    pub fn duration_secs(&self) -> f64 {
        if self.timescale == 0 {
            0.0
        } else {
            self.duration as f64 / self.timescale as f64
        }
    }
    pub fn video(&self) -> Option<&StreamInfo> {
        self.streams
            .iter()
            .find(|s| s.handler == "vide" || s.codec_fourcc.starts_with("avc"))
    }
}

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn be16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}

fn be64(b: &[u8], o: usize) -> u64 {
    u64::from_be_bytes([
        b[o],
        b[o + 1],
        b[o + 2],
        b[o + 3],
        b[o + 4],
        b[o + 5],
        b[o + 6],
        b[o + 7],
    ])
}

fn fourcc(b: &[u8], o: usize) -> String {
    String::from_utf8_lossy(&b[o..o + 4]).into_owned()
}

fn is_container(typ: &str) -> bool {
    matches!(
        typ,
        "moov" | "trak" | "mdia" | "minf" | "stbl" | "edts" | "dinf" | "udta" | "mvex" | "moof"
            | "traf" | "mfra" | "meta"
    )
}

/// Does this box consist of child boxes only, with no version/flags word?
fn is_plain_container(typ: &str) -> bool {
    matches!(
        typ,
        "trak" | "mdia" | "minf" | "stbl" | "edts" | "dinf" | "moov" | "udta"
    )
}

struct Walker<'a> {
    b: &'a [u8],
    log: Vec<String>,
    streams: Vec<StreamInfo>,
    cur: Option<usize>,
    mvhd: Option<(u32, u64)>,
}

impl<'a> Walker<'a> {
    fn note(&mut self, depth: usize, msg: String) {
        self.log.push(format!("{}{}", "  ".repeat(depth), msg));
    }

    fn walk(&mut self, start: usize, end: usize, depth: usize) {
        let mut p = start;
        while p + 8 <= end {
            let mut size = be32(self.b, p) as u64;
            let typ = fourcc(self.b, p + 4);
            let mut hdr = 8usize;
            if size == 1 {
                if p + 16 > end {
                    break;
                }
                size = be64(self.b, p + 8);
                hdr = 16;
            } else if size == 0 {
                size = (end - p) as u64;
            }
            if size < hdr as u64 || p as u64 + size > end as u64 {
                self.note(depth, format!("<malformed box header for '{}' at {}>", typ, p));
                break;
            }
            let (ps, pe) = (p + hdr, p + size as usize);
            self.handle(&typ, ps, pe, depth);
            p += size as usize;
        }
    }

    fn handle(&mut self, typ: &str, ps: usize, pe: usize, depth: usize) {
        self.note(depth, format!("{} (payload {} bytes)", typ, pe - ps));

        match typ {
            "ftyp" => {
                self.note(depth + 1, format!("major_brand={}", fourcc(self.b, ps)));
            }
            "mvhd" => {
                let v = self.b[ps];
                let (ts, dur) = if v == 1 {
                    (be32(self.b, ps + 20), be64(self.b, ps + 24))
                } else {
                    (be32(self.b, ps + 12), be32(self.b, ps + 16) as u64)
                };
                self.mvhd = Some((ts, dur));
                self.note(
                    depth + 1,
                    format!("version={} timescale={} duration={}", v, ts, dur),
                );
            }
            "mdhd" => {
                let v = self.b[ps];
                let (ts, dur) = if v == 1 {
                    (be32(self.b, ps + 20), be64(self.b, ps + 24))
                } else {
                    (be32(self.b, ps + 12), be32(self.b, ps + 16) as u64)
                };
                if let Some(i) = self.cur {
                    self.streams[i].timescale = ts;
                    self.streams[i].duration = dur;
                }
                self.note(
                    depth + 1,
                    format!("version={} timescale={} duration={}", v, ts, dur),
                );
            }
            "hdlr" => {
                let h = fourcc(self.b, ps + 8);
                if let Some(i) = self.cur {
                    self.streams[i].handler = h.clone();
                }
                self.note(depth + 1, format!("handler_type={}", h));
            }
            "stsz" => {
                let sample_size = be32(self.b, ps + 4);
                let count = be32(self.b, ps + 8);
                let total: u64 = if sample_size == 0 {
                    (0..count as usize)
                        .map(|i| be32(self.b, ps + 12 + i * 4) as u64)
                        .sum()
                } else {
                    sample_size as u64 * count as u64
                };
                if let Some(i) = self.cur {
                    self.streams[i].sample_count = count;
                    self.streams[i].sample_bytes = total;
                }
                self.note(
                    depth + 1,
                    format!(
                        "sample_size={} sample_count={} total_sample_bytes={}",
                        sample_size, count, total
                    ),
                );
            }
            "stss" => {
                let n = be32(self.b, ps + 4);
                if let Some(i) = self.cur {
                    self.streams[i].sync_samples = Some(n);
                }
                self.note(depth + 1, format!("sync_sample_count={}", n));
            }
            "stco" | "co64" => {
                let n = be32(self.b, ps + 4);
                if let Some(i) = self.cur {
                    self.streams[i].chunk_count = Some(n);
                }
                self.note(depth + 1, format!("chunk_count={}", n));
            }
            "stts" => {
                let n = be32(self.b, ps + 4);
                self.note(depth + 1, format!("time_to_sample_entries={}", n));
                for i in 0..n.min(4) as usize {
                    let o = ps + 8 + i * 8;
                    self.note(
                        depth + 2,
                        format!("entry[{}] count={} delta={}", i, be32(self.b, o), be32(self.b, o + 4)),
                    );
                }
            }
            "stsd" => {
                let n = be32(self.b, ps + 4);
                self.note(depth + 1, format!("entry_count={}", n));
                self.walk(ps + 8, pe, depth + 1);
                return;
            }
            "avc1" | "avc3" => {
                let w = be16(self.b, ps + 24);
                let h = be16(self.b, ps + 26);
                if let Some(i) = self.cur {
                    self.streams[i].codec_fourcc = typ.to_string();
                    self.streams[i].width = w;
                    self.streams[i].height = h;
                }
                self.note(
                    depth + 1,
                    format!("fourcc={} width={} height={}", typ, w, h),
                );
                self.walk(ps + 78, pe, depth + 1);
                return;
            }
            "avcC" => {
                let cfg = self.b[ps..pe].to_vec();
                self.note(
                    depth + 1,
                    format!(
                        "configurationVersion={} AVCProfileIndication={} profile_compatibility={} AVCLevelIndication={} lengthSizeMinusOne={} numOfSPS={}",
                        cfg.first().copied().unwrap_or(0),
                        cfg.get(1).copied().unwrap_or(0),
                        cfg.get(2).copied().unwrap_or(0),
                        cfg.get(3).copied().unwrap_or(0),
                        cfg.get(4).copied().unwrap_or(0) & 0x03,
                        cfg.get(5).copied().unwrap_or(0) & 0x1f,
                    ),
                );
                if let Some(i) = self.cur {
                    self.streams[i].avcc = Some(cfg);
                }
            }
            _ => {}
        }

        if typ == "trak" {
            self.streams.push(StreamInfo::default());
            let idx = self.streams.len() - 1;
            let saved = self.cur;
            self.cur = Some(idx);
            self.walk(ps, pe, depth + 1);
            self.cur = saved;
        } else if is_container(typ) {
            let start = if is_plain_container(typ) { ps } else { ps + 4 };
            self.walk(start, pe, depth + 1);
        }
    }
}

pub fn parse(bytes: &[u8]) -> Result<Mp4Info, String> {
    if bytes.len() < 16 {
        return Err("file too small to be an MP4".into());
    }
    let mut info = Mp4Info::default();

    // ---- top level ----
    let mut tops: Vec<(String, usize, usize, u64)> = Vec::new();
    let mut p = 0usize;
    while p + 8 <= bytes.len() {
        let mut size = be32(bytes, p) as u64;
        let typ = fourcc(bytes, p + 4);
        let mut hdr = 8usize;
        if size == 1 {
            size = be64(bytes, p + 8);
            hdr = 16;
        } else if size == 0 {
            size = (bytes.len() - p) as u64;
        }
        if size < hdr as u64 || p as u64 + size > bytes.len() as u64 {
            return Err(format!(
                "malformed top-level box '{}' at offset {} (size {})",
                typ, p, size
            ));
        }
        info.top_level_boxes.push(typ.clone());
        info.other_top_level_boxes.push((typ.clone(), size));
        tops.push((typ.clone(), p + hdr, p + size as usize, size));
        match typ.as_str() {
            "ftyp" => {
                info.has_ftyp = true;
                let ps = p + hdr;
                info.major_brand = fourcc(bytes, ps);
                let mut o = ps + 8;
                while o + 4 <= p + size as usize {
                    info.compatible_brands.push(fourcc(bytes, o));
                    o += 4;
                }
            }
            "moov" => info.has_moov = true,
            "mdat" => {
                info.has_mdat = true;
                info.mdat_count += 1;
                info.mdat_bytes += size - hdr as u64;
            }
            _ => {}
        }
        p += size as usize;
    }

    if !info.has_ftyp || !info.has_moov || !info.has_mdat {
        return Err(format!(
            "required top-level boxes missing: ftyp={} moov={} mdat={}",
            info.has_ftyp, info.has_moov, info.has_mdat
        ));
    }

    // ---- deep walk of moov (this is where mvhd/mdhd/stsz/hdlr live) ----
    for (typ, ps, pe, _s) in &tops {
        if typ == "moov" {
            let mut w = Walker {
                b: bytes,
                log: Vec::new(),
                streams: Vec::new(),
                cur: None,
                mvhd: None,
            };
            w.note(0, format!("moov (payload {} bytes)", pe - ps));
            w.walk(*ps, *pe, 1);
            info.streams = w.streams;
            info.log = w.log;
            if let Some((ts, dur)) = w.mvhd {
                info.timescale = ts;
                info.duration = dur;
            }
        }
    }

    Ok(info)
}
