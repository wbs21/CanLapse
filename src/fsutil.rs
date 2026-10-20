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
/// Windows 上 `rename` 对已存在的目标不一定成功，因此失败时只在目标确实存在的情况下
/// 删掉旧文件再重试。旧文件此时通常是上一次失败留下的残片；正常流程里目标不应存在。
pub fn replace_file(tmp: &Path, dest: &Path) -> io::Result<()> {
    match fs::rename(tmp, dest) {
        Ok(()) => Ok(()),
        Err(first) => {
            if !dest.exists() {
                return Err(first);
            }
            fs::remove_file(dest)?;
            fs::rename(tmp, dest)
        }
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
        write_atomic(&path, b"{\"ok\":true}").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"{\"ok\":true}");
        assert!(!tmp_path(&path).exists());
        assert_ne!(tmp_path(&path), tmp_path(&path));
        let _ = fs::remove_dir_all(&dir);
    }
}
