//! 录制会话：缓存目录、流式 MP4、packet 索引与 `markers.json`。
//!
//! 目录命名按需求：`{时间}_{会话名称}`，例如 `2026-02-14_13-20-51_下雨天喝咖啡`。
//! 新会话保存 `stream.mp4` + `stream.packets`，不再生成图片帧；
//! 旧会话的 `frame_0000.png/jpg` 仍保持可读和可重新导出。

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::frame::Frame;
use crate::img_io;
use crate::win;

/// New recordings store encoded packets here instead of image frames.
pub const STREAM_FILE: &str = "stream.mp4";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Marker {
    /// 标记发生在第几帧之后（0 表示第一帧之前）
    pub frame: u32,
    /// 本地时钟时间
    pub clock: String,
    /// 距录制开始的毫秒数
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SessionMeta {
    pub version: u32,
    pub name: String,
    pub created: String,
    pub source_title: String,
    pub source_class: String,
    pub source_process: String,
    pub width: i32,
    pub height: i32,
    /// 录制帧密度（快照采样频率），与导出播放帧率无关
    pub record_fps: f64,
    /// 像素差异阈值
    pub threshold: f32,
    pub frames: u32,
    pub dropped: u32,
    pub duration_ms: u64,
    /// 录制是否正常结束（false = 目标窗口关闭等异常终止）
    pub completed: bool,
}

#[derive(Serialize, Deserialize)]
pub struct MarkersFile {
    pub session: SessionMeta,
    pub markers: Vec<Marker>,
}

pub struct Session {
    dir: PathBuf,
    meta: SessionMeta,
    markers: Vec<Marker>,
}

/// 历史缓存会话的摘要（历史面板用：会话名称｜帧数量）
#[derive(Clone, Debug)]
pub struct SessionSummary {
    pub dir: PathBuf,
    pub name: String,
    pub frames: u32,
    pub created: String,
    /// markers.json 缺失时视为未知但可兼容的旧会话，按完成处理。
    pub completed: bool,
}

impl Session {
    pub fn create(
        root: &Path,
        name: &str,
        info: &win::WindowInfo,
        width: i32,
        height: i32,
        record_fps: f64,
        threshold: f32,
    ) -> Result<Session, String> {
        let clean = win::sanitize_name(name);
        let stamp = win::stamp_folder(&win::local_now());
        let mut dir = root.join(format!("{stamp}_{clean}"));
        // 同一秒内重名时退让，不覆盖已有缓存
        let mut n = 2;
        while dir.exists() {
            dir = root.join(format!("{stamp}_{clean}_{n}"));
            n += 1;
        }
        fs::create_dir_all(&dir)
            .map_err(|e| format!("创建缓存目录 {} 失败: {e}", dir.display()))?;
        let meta = SessionMeta {
            version: 1,
            name: clean,
            created: win::stamp_iso(&win::local_now()),
            source_title: info.title.clone(),
            source_class: info.class_name.clone(),
            source_process: info.process.clone(),
            width,
            height,
            record_fps,
            threshold,
            frames: 0,
            dropped: 0,
            duration_ms: 0,
            completed: false,
        };
        Ok(Session {
            dir,
            meta,
            markers: Vec::new(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 存一帧，返回保存后的帧总数。
    /// 单帧失败由调用方决定怎么处理（需求：跳过该帧、不中断录制）。
    pub fn save_frame(&mut self, frame: &Frame) -> Result<u32, String> {
        let n = self.meta.frames;
        // 近空白帧用 PNG（几 KB；JPEG 反而更大），有内容的帧用 JPEG q85（约为 PNG 的 1/5）
        let (path, use_png) = if frame.is_blank() {
            (self.dir.join(format!("frame_{n:04}.png")), true)
        } else {
            (self.dir.join(format!("frame_{n:04}.jpg")), false)
        };
        if use_png {
            img_io::write_png(&path, frame)
                .map_err(|e| format!("写入 {} 失败: {e}", path.display()))?;
        } else {
            img_io::write_jpeg(&path, frame)?;
        }
        self.meta.frames += 1;
        Ok(self.meta.frames)
    }

    /// Count a frame already encoded by the streaming writer.
    pub fn record_encoded_frame(&mut self) -> u32 {
        self.meta.frames += 1;
        self.meta.frames
    }

    /// 打一个标记（来自悬浮条的 🏷 按钮，或命令行的 F8）。
    ///
    /// `Marker::frame` 存的是**打标记那一刻已保存的帧数**，也就是
    /// "下一个将要保存的帧的序号"，**不是**指向某个已存在的 frame_XXXX.png。
    /// 例如只存了 `frame_0000.png` 时打标记，写的是 `frame: 1` —— 这是自洽的
    /// （标记落在"当前内容之后"），但容易被误读成"指到 frame_0001.png"。
    /// 想把标记映射到视频时间，应当用 `Marker::elapsed_ms`（与导出帧率一起换算），
    /// 而不是这个字段。
    pub fn add_marker(&mut self, note: Option<String>, elapsed_ms: u64) {
        self.markers.push(Marker {
            frame: self.meta.frames,
            clock: win::stamp_iso(&win::local_now()),
            elapsed_ms,
            note,
        });
        // 每次打标记都落盘：崩溃/断电时不至于丢掉标记
        let _ = self.write_markers();
    }

    pub fn marker_count(&self) -> usize {
        self.markers.len()
    }

    /// 录制过程中目标窗口尺寸变了：更新元信息，后续重新导出按新尺寸处理
    pub fn update_geometry(&mut self, width: i32, height: i32) {
        self.meta.width = width;
        self.meta.height = height;
    }

    /// 收尾：写入最终的 markers.json（含会话元信息）
    pub fn finish(
        &mut self,
        dropped: u32,
        duration_ms: u64,
        completed: bool,
    ) -> Result<(), String> {
        self.meta.dropped = dropped;
        self.meta.duration_ms = duration_ms;
        self.meta.completed = completed;
        self.write_markers()
    }

    fn write_markers(&self) -> Result<(), String> {
        let payload = MarkersFile {
            session: self.meta.clone(),
            markers: self.markers.clone(),
        };
        let text = serde_json::to_string_pretty(&payload)
            .map_err(|e| format!("序列化 markers.json 失败: {e}"))?;
        let path = self.dir.join("markers.json");
        crate::fsutil::write_atomic(&path, text.as_bytes())
            .map_err(|e| format!("写入 {} 失败: {e}", path.display()))
    }

    /// 列出某个根目录下的所有会话（历史缓存会话面板用）
    pub fn list(root: &Path) -> Vec<SessionSummary> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(root) else {
            return out;
        };
        for e in entries.flatten() {
            let dir = e.path();
            let Ok(meta) = fs::symlink_metadata(&dir) else {
                continue;
            };
            use std::os::windows::fs::MetadataExt;
            const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
            if !meta.file_type().is_dir()
                || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            {
                continue;
            }
            let (name, created, meta_frames, completed) = read_markers_meta(&dir);
            let frames = meta_frames
                .filter(|n| *n > 0)
                .unwrap_or_else(|| count_frames(&dir));
            let name = name.unwrap_or_else(|| {
                dir.file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default()
            });
            let has_stream = dir.join(STREAM_FILE).is_file();
            out.push(SessionSummary {
                dir,
                name,
                frames,
                created: created.unwrap_or_default(),
                // 旧图片会话没有 markers.json 仍按兼容会话处理；新流式会话没有
                // markers.json 通常表示录制中途崩溃，不能标记成正常完成。
                completed: completed.unwrap_or(!has_stream),
            });
        }
        // 目录名以时间开头，倒序即最新在前
        out.sort_by(|a, b| b.dir.cmp(&a.dir));
        out
    }

    /// 只允许删除缓存根目录的**直接子目录**。删除是递归的，必须防止
    /// CANLAPSE_CACHE 被指向别处、或缓存目录里出现 junction 时误删外部内容。
    pub fn delete_in(root: &Path, dir: &Path) -> Result<(), String> {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        let link_meta = fs::symlink_metadata(dir)
            .map_err(|e| format!("读取会话目录 {} 失败: {e}", dir.display()))?;
        if link_meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(format!("拒绝删除重解析点/junction：{}", dir.display()));
        }
        let root = fs::canonicalize(root)
            .map_err(|e| format!("解析缓存根目录 {} 失败: {e}", root.display()))?;
        let target = fs::canonicalize(dir)
            .map_err(|e| format!("解析会话目录 {} 失败: {e}", dir.display()))?;
        if target.parent() != Some(root.as_path()) {
            return Err(format!(
                "拒绝删除缓存根目录之外的路径：{}",
                target.display()
            ));
        }
        fs::remove_dir_all(&target).map_err(|e| format!("删除 {} 失败: {e}", target.display()))
    }
}

fn count_frames(dir: &Path) -> u32 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| {
            let is_file = e.file_type().map(|t| t.is_file()).unwrap_or(false);
            if !is_file {
                return false;
            }
            let n = e.file_name().to_string_lossy().to_ascii_lowercase();
            n.starts_with("frame_")
                && (n.ends_with(".png") || n.ends_with(".jpg") || n.ends_with(".jpeg"))
        })
        .count() as u32
}

fn read_markers_meta(dir: &Path) -> (Option<String>, Option<String>, Option<u32>, Option<bool>) {
    let path = dir.join("markers.json");
    if let Ok(meta) = fs::metadata(&path) {
        if meta.len() > 4 * 1024 * 1024 {
            return (None, None, None, None);
        }
    }
    let Ok(text) = fs::read_to_string(&path) else {
        return (None, None, None, None);
    };
    match serde_json::from_str::<MarkersFile>(&text) {
        Ok(m) => (
            Some(m.session.name),
            Some(m.session.created),
            Some(m.session.frames),
            Some(m.session.completed),
        ),
        Err(_) => (None, None, None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Frame;
    use crate::img_io;
    use windows_sys::Win32::Foundation::RECT;

    fn tmp_root(tag: &str) -> PathBuf {
        // 用**工作区内**的目录而不是 std::env::temp_dir()：
        // 系统临时目录在受限令牌/被清理的环境下会建不了目录，导致测试假失败（实测踩过）。
        let p = PathBuf::from("target").join("test-tmp").join(format!(
            "canlapse-test-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn fake_info() -> win::WindowInfo {
        win::WindowInfo {
            hwnd: std::ptr::null_mut(),
            title: "测试窗口".into(),
            class_name: "TestClass".into(),
            pid: 0,
            process: "test.exe".into(),
            rect: RECT {
                left: 0,
                top: 0,
                right: 4,
                bottom: 4,
            },
            client: RECT {
                left: 0,
                top: 0,
                right: 4,
                bottom: 4,
            },
            minimized: false,
        }
    }

    /// 这条测试覆盖需求里的缓存格式：`frame_xxxx.png` + `markers.json`（帧序号/时间/备注）
    #[test]
    fn 会话存帧写标记并可被列出与删除() {
        let root = tmp_root("session");
        let info = fake_info();
        let mut s = Session::create(&root, "测试会话", &info, 4, 4, 2.0, 0.01).unwrap();
        assert!(s
            .dir()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with("_测试会话"));

        let f = Frame::new(4, 4, vec![0u8; 64]);
        assert_eq!(s.save_frame(&f).unwrap(), 1);
        assert_eq!(s.save_frame(&f).unwrap(), 2);
        s.add_marker(Some("第一笔".into()), 1200); // 第 2 帧之后打的标记
        assert_eq!(s.save_frame(&f).unwrap(), 3);
        s.add_marker(None, 2500);
        s.finish(1, 3000, true).unwrap();

        let text = fs::read_to_string(s.dir().join("markers.json")).unwrap();
        let parsed: MarkersFile = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.session.version, 1);
        assert_eq!(parsed.session.frames, 3);
        assert_eq!(parsed.session.dropped, 1);
        assert_eq!(parsed.session.duration_ms, 3000);
        assert!(parsed.session.completed);
        assert_eq!(parsed.session.record_fps, 2.0);
        assert_eq!(parsed.session.threshold, 0.01);
        assert_eq!((parsed.session.width, parsed.session.height), (4, 4));
        assert_eq!(parsed.session.source_process, "test.exe");

        assert_eq!(parsed.markers.len(), 2);
        assert_eq!(parsed.markers[0].frame, 2);
        assert_eq!(parsed.markers[0].elapsed_ms, 1200);
        assert_eq!(parsed.markers[0].note.as_deref(), Some("第一笔"));
        assert_eq!(parsed.markers[1].frame, 3);
        assert_eq!(parsed.markers[1].elapsed_ms, 2500);
        assert!(parsed.markers[1].note.is_none());

        // 帧文件确实是可读的 PNG，且尺寸正确
        let png = s.dir().join("frame_0000.png");
        assert!(png.exists());
        let (w, h, _) = img_io::read_png_rgba(&png).unwrap();
        assert_eq!((w, h), (4, 4));

        // 历史缓存会话列表（规格里的「会话名称｜帧数量」）
        let list = Session::list(&root);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].frames, 3);
        assert_eq!(list[0].name, "测试会话");

        // 删除会带走整个会话目录
        Session::delete_in(&root, s.dir()).unwrap();
        assert!(!png.exists());
        assert_eq!(Session::list(&root).len(), 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 同一秒内重名会话不会互相覆盖() {
        let root = tmp_root("dup");
        let info = fake_info();
        let a = Session::create(&root, "同名", &info, 4, 4, 2.0, 0.01).unwrap();
        let b = Session::create(&root, "同名", &info, 4, 4, 2.0, 0.01).unwrap();
        assert_ne!(a.dir(), b.dir());
        assert_eq!(Session::list(&root).len(), 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 无元数据的流式会话标记为未完成() {
        let root = tmp_root("stream-incomplete");
        let dir = root.join("2026-01-01_00-00-00_test");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(STREAM_FILE), b"partial").unwrap();
        let list = Session::list(&root);
        assert_eq!(list.len(), 1);
        assert!(!list[0].completed);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 拒绝删除缓存根目录之外的目录() {
        let root = tmp_root("delete-root");
        let outside = tmp_root("delete-outside");
        let err = Session::delete_in(&root, &outside).unwrap_err();
        assert!(err.contains("缓存根目录之外"));
        assert!(outside.exists());
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }
}
