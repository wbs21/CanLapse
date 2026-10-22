//! 小文件原子写入工具。
//!
//! 帧、markers.json、MP4 都不能直接写最终路径：磁盘满、进程崩溃或单帧编码失败时
//! 会留下“看起来存在、实际是半截”的文件。对帧缓存来说这还会制造同序号的
//! `frame_N.png` + `frame_N.jpg`，导出时读到坏文件就整段失败。
//!
//! 这里统一走“同目录临时文件 → rename 到最终名”。临时文件必须与目标同目录，
//! 跨卷 rename 不是原子操作。

use std::fs;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// 目标路径对应的唯一临时路径。加入 PID 和进程内序号，避免两个 CLI 实例
/// 同时导出同一目标时互相截断；`create_new` 再保证不会跟随已存在的链接。
pub fn tmp_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    PathBuf::from(s)
}

/// 创建唯一临时文件。若极小概率撞名，重试几次而不是覆盖已有文件。
pub fn create_temp(path: &Path) -> io::Result<(PathBuf, File)> {
    for _ in 0..16 {
        let tmp = tmp_path(path);
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(f) => return Ok((tmp, f)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "无法创建唯一临时文件",
    ))
}

/// 把已经写好的临时文件替换到最终路径。
///
/// 使用 `MoveFileExW(MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)`，
/// 避免“先删旧文件、再 rename”时第二次操作失败而把原文件也丢掉。
pub fn replace_file(tmp: &Path, dest: &Path) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let from = crate::win::wide(&tmp.to_string_lossy());
    let to = crate::win::wide(&dest.to_string_lossy());
    let ok = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok != 0 {
        return Ok(());
    }
    // Some network/virtual filesystems reject WRITE_THROUGH even though the
    // atomic replacement itself is supported. Retry without that hint while
    // still never deleting the destination first.
    let ok = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_REPLACE_EXISTING) };
    if ok != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// 原子写入一小段数据（markers.json、JPEG 等）。
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let (tmp, mut file) = create_temp(path)?;
    if let Err(e) = file.write_all(data) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    drop(file);
    if let Err(e) = replace_file(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// 录制/导出开始前的最低可用空间。低于这个值直接拒绝开始，避免录到一半才发现。
pub const MIN_FREE_BYTES: u64 = 512 * 1024 * 1024;

/// 单个目录下清理遗留临时文件的默认年龄。
pub const STALE_TEMP_AGE: Duration = Duration::from_secs(24 * 60 * 60);

pub fn ensure_headroom(path: &Path, minimum: u64, purpose: &str) -> Result<(), String> {
    if let Some(free) = crate::win::disk_free_bytes(path) {
        if free < minimum {
            return Err(format!(
                "{purpose}可用空间不足：{} 字节可用，至少需要 {} 字节",
                free, minimum
            ));
        }
    }
    Ok(())
}

/// 清理缓存/输出目录中长时间未修改的 `.tmp`/`.part` 残留。
///
/// 只删除超过 `max_age` 的文件，避免误删正在写入的临时文件。返回删除数量。
pub fn cleanup_stale_temps(root: &Path, max_age: Duration) -> usize {
    fn walk(dir: &Path, max_age: Duration, removed: &mut usize) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if ft.is_dir() {
                use std::os::windows::fs::MetadataExt;
                const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
                // 不跟随 junction / symlink，避免清理任务越出缓存根目录。
                if fs::symlink_metadata(&path)
                    .map(|m| m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
                    .unwrap_or(true)
                {
                    continue;
                }
                walk(&path, max_age, removed);
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if !name.ends_with(".tmp") && !name.ends_with(".part") {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let Ok(modified) = meta.modified() else {
                continue;
            };
            let Ok(age) = SystemTime::now().duration_since(modified) else {
                continue;
            };
            if age >= max_age && fs::remove_file(&path).is_ok() {
                *removed += 1;
            }
        }
    }

    let mut removed = 0;
    walk(root, max_age, &mut removed);
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 原子写入内容正确且不留下临时文件() {
        let dir = PathBuf::from("target")
            .join("test-tmp")
            .join(format!("canlapse-fsutil-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("markers.json");
        fs::write(&path, b"old").unwrap();
        write_atomic(&path, b"{\"ok\":true}").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"{\"ok\":true}");
        assert!(!tmp_path(&path).exists());
        assert_ne!(tmp_path(&path), tmp_path(&path));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn 只清理过期临时文件() {
        let dir = PathBuf::from("target")
            .join("test-tmp")
            .join(format!("canlapse-temp-clean-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let stale = dir.join("old.tmp");
        let fresh = dir.join("fresh.tmp");
        let keep = dir.join("keep.txt");
        fs::write(&stale, b"old").unwrap();
        fs::write(&fresh, b"new").unwrap();
        fs::write(&keep, b"keep").unwrap();
        assert_eq!(cleanup_stale_temps(&dir, Duration::ZERO), 2);
        assert!(!stale.exists() && !fresh.exists());
        assert!(keep.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
