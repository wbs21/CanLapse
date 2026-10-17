//! 录制会话：缓存目录、`frame_xxxx.png`、`markers.json`。
//!
//! 目录命名按需求：`{时间}_{会话名称}`，例如 `2026-02-14_13-20-51_下雨天喝咖啡`。
//! 帧文件命名：`frame_0000.png`（四位序号，超过 9999 自动变宽）。

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::frame::Frame;
use crate::png_io;
use crate::win;

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
        fs::create_dir_all(&dir).map_err(|e| format!("创建缓存目录 {} 失败: {e}", dir.display()))?;
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
        Ok(Session { dir, meta, markers: Vec::new() })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn frame_count(&self) -> u32 {
        self.meta.frames
    }

    /// 存一帧，返回保存后的帧总数。
    /// 单帧失败由调用方决定怎么处理（需求：跳过该帧、不中断录制）。
    pub fn save_frame(&mut self, frame: &Frame) -> Result<u32, String> {
        let path = self.dir.join(format!("frame_{:04}.png", self.meta.frames));
        png_io::write_png(&path, frame).map_err(|e| format!("写入 {} 失败: {e}", path.display()))?;
        self.meta.frames += 1;
        Ok(self.meta.frames)
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
    pub fn finish(&mut self, dropped: u32, duration_ms: u64, completed: bool) -> Result<(), String> {
        self.meta.dropped = dropped;
        self.meta.duration_ms = duration_ms;
        self.meta.completed = completed;
        self.write_markers()
    }

    fn write_markers(&self) -> Result<(), String> {
        let payload = MarkersFile { session: self.meta.clone(), markers: self.markers.clone() };
        let text = serde_json::to_string_pretty(&payload)
            .map_err(|e| format!("序列化 markers.json 失败: {e}"))?;
        let path = self.dir.join("markers.json");
        fs::write(&path, text).map_err(|e| format!("写入 {} 失败: {e}", path.display()))
    }

    /// 列出某个根目录下的所有会话（历史缓存会话面板用）
    pub fn list(root: &Path) -> Vec<SessionSummary> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(root) else {
            return out;
        };
        for e in entries.flatten() {
            let dir = e.path();
            if !dir.is_dir() {
                continue;
            }
            let frames = count_frames(&dir);
            let (name, created) = read_markers_meta(&dir);
            let name = name.unwrap_or_else(|| {
                dir.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
            });
            out.push(SessionSummary {
                dir,
                name,
                frames,
                created: created.unwrap_or_default(),
            });
        }
        // 目录名以时间开头，倒序即最新在前
        out.sort_by(|a, b| b.dir.cmp(&a.dir));
        out
    }

    /// 删除整个会话缓存目录
    pub fn delete(dir: &Path) -> Result<(), String> {
        fs::remove_dir_all(dir).map_err(|e| format!("删除 {} 失败: {e}", dir.display()))
    }
}

fn count_frames(dir: &Path) -> u32 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.starts_with("frame_") && n.ends_with(".png")
        })
        .count() as u32
}

fn read_markers_meta(dir: &Path) -> (Option<String>, Option<String>) {
    let path = dir.join("markers.json");
    let Ok(text) = fs::read_to_string(&path) else {
        return (None, None);
    };
    match serde_json::from_str::<MarkersFile>(&text) {
        Ok(m) => (Some(m.session.name), Some(m.session.created)),
        Err(_) => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Frame;
    use crate::png_io;
    use windows_sys::Win32::Foundation::RECT;

    fn tmp_root(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("canlapse-test-{}-{}", std::process::id(), tag));
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
            rect: RECT { left: 0, top: 0, right: 4, bottom: 4 },
            client: RECT { left: 0, top: 0, right: 4, bottom: 4 },
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
        let (w, h, _) = png_io::read_png_rgba(&png).unwrap();
        assert_eq!((w, h), (4, 4));

        // 历史缓存会话列表（规格里的「会话名称｜帧数量」）
        let list = Session::list(&root);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].frames, 3);
        assert_eq!(list[0].name, "测试会话");

        // 删除会带走整个会话目录
        Session::delete(s.dir()).unwrap();
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
}
