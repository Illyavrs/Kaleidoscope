//! Linux lazer is distributed as an AppImage. The managed host needs the real
//! installation tree, so a selected AppImage is extracted once with its own
//! `--appimage-extract` into a private, hash-named folder. The extracted files
//! are never trusted: the launcher still verifies every assembly hash before
//! each launch.

use crate::Result;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use uuid::Uuid;

const EXTRACT_TIMEOUT: Duration = Duration::from_secs(180);
const ENTRY: &str = "usr/bin/osu!";

pub fn is_appimage(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".appimage"))
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|_| "無法讀取 AppImage")?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|_| "無法讀取 AppImage")?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Extract `appimage` below `root` and return the extracted `osu!` executable.
/// An already extracted copy of the same file (by SHA-256) is reused.
pub fn extract(appimage: &Path, root: &Path) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(appimage).map_err(|_| "找不到選擇的 AppImage")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("請選擇 AppImage 檔案本身，而不是捷徑或資料夾".into());
    }
    if metadata.permissions().mode() & 0o111 == 0 {
        return Err("AppImage 尚未設為可執行；請先執行 chmod +x 再選擇".into());
    }
    let hash = sha256_file(appimage)?;
    let destination = root.join(&hash);
    let entry = destination.join(ENTRY);
    if entry.is_file() {
        return Ok(entry);
    }
    if destination.exists() {
        return Err("AppImage 解壓縮資料夾不完整；請刪除後重試".into());
    }
    fs::create_dir_all(root).map_err(|_| "無法建立 AppImage 解壓縮資料夾")?;
    fs::set_permissions(root, fs::Permissions::from_mode(0o700))
        .map_err(|_| "無法設定 AppImage 解壓縮資料夾權限")?;
    let staging = root.join(format!(".extract-{}", Uuid::new_v4().simple()));
    fs::create_dir(&staging).map_err(|_| "無法建立暫存資料夾")?;
    let result = run_extract(appimage, &staging).and_then(|()| {
        let tree = staging.join("squashfs-root");
        if !tree.join(ENTRY).is_file() {
            return Err("AppImage 內找不到 usr/bin/osu!，這不是 lazer 套件".into());
        }
        fs::rename(&tree, &destination).map_err(|_| "無法保存解壓縮後的 lazer".to_owned())
    });
    let _ = fs::remove_dir_all(&staging);
    result?;
    Ok(entry)
}

fn run_extract(appimage: &Path, staging: &Path) -> Result<()> {
    // The AppImage runtime handles --appimage-extract before any application code.
    let mut child = Command::new(appimage)
        .arg("--appimage-extract")
        .current_dir(staging)
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "無法執行 AppImage 進行解壓縮")?;
    let deadline = Instant::now() + EXTRACT_TIMEOUT;
    loop {
        match child.try_wait().map_err(|_| "無法查詢解壓縮狀態")? {
            Some(status) if status.success() => return Ok(()),
            Some(_) => return Err("AppImage 解壓縮失敗".into()),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("AppImage 解壓縮逾時".into());
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("kaleidoscope-appimage-{}", Uuid::new_v4().simple()));
        fs::create_dir(&path).unwrap();
        fs::canonicalize(path).unwrap()
    }

    fn fake_appimage(directory: &Path, body: &str, executable: bool) -> PathBuf {
        let path = directory.join("osu.AppImage");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mode = if executable { 0o755 } else { 0o644 };
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    const GOOD: &str = "[ \"$1\" = --appimage-extract ] || exit 2; mkdir -p squashfs-root/usr/bin && echo fixture > 'squashfs-root/usr/bin/osu!'";

    #[test]
    fn recognises_appimage_names_case_insensitively() {
        assert!(is_appimage(Path::new("/x/osu.AppImage")));
        assert!(is_appimage(Path::new("/x/osu.appimage")));
        assert!(!is_appimage(Path::new("/x/osu!")));
    }

    #[test]
    fn extracts_once_and_reuses_the_hash_named_tree() {
        let directory = scratch();
        let image = fake_appimage(&directory, GOOD, true);
        let root = directory.join("store");
        let first = extract(&image, &root).unwrap();
        assert!(first.ends_with("usr/bin/osu!") && first.is_file());
        assert!(first.starts_with(&root));
        assert!(fs::read_dir(&root).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".extract-")));
        let again = extract(&image, &root).unwrap();
        assert_eq!(first, again);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn refuses_non_executable_failing_foreign_and_linked_images() {
        let directory = scratch();
        let root = directory.join("store");
        let plain = fake_appimage(&directory, GOOD, false);
        assert!(extract(&plain, &root).unwrap_err().contains("chmod +x"));
        let failing = fake_appimage(&directory, "exit 1", true);
        assert!(extract(&failing, &root).is_err());
        let foreign = fake_appimage(
            &directory,
            "mkdir -p squashfs-root/usr/bin && echo x > squashfs-root/usr/bin/other",
            true,
        );
        assert!(extract(&foreign, &root)
            .unwrap_err()
            .contains("usr/bin/osu!"));
        // Failed attempts leave no staging directories or partial trees behind.
        let leftovers: Vec<_> = fs::read_dir(&root).unwrap().collect();
        assert!(leftovers.is_empty());
        let link = directory.join("link.AppImage");
        std::os::unix::fs::symlink(&failing, &link).unwrap();
        assert!(extract(&link, &root).is_err());
        fs::remove_dir_all(directory).unwrap();
    }
}
