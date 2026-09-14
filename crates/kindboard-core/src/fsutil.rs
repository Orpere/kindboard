//! Small filesystem helpers shared by `kubeconfig` and `state`.
//!
//! Implements the crash-safe write discipline from ADR-0007: serialize to a
//! temporary file in the *same directory* as the target, fsync, then rename
//! over the target. The previous content is copied to `<target>.bak` first so
//! an interrupted write can never corrupt the last-good file.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` to `path` atomically.
///
/// Steps: ensure the parent directory exists, copy any existing target to
/// `<path>.bak`, write a sibling temp file, fsync it, rename it over the
/// target. Never leaves a partially-written target behind.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_write_impl(path, bytes, false)
}

/// Like [`atomic_write`], but the created file gets mode `0600` before any
/// bytes land on disk — used for secrets-bearing files (kubeconfig).
pub(crate) fn atomic_write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_write_impl(path, bytes, true)
}

fn atomic_write_impl(path: &Path, bytes: &[u8], private: bool) -> io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    if path.is_file() {
        backup(path)?;
    }
    let (mut file, tmp) = create_new_temp(path)?;
    if private {
        set_private_mode(&file)?;
    }
    {
        use std::io::Write;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    drop(file);
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&tmp);
            Err(err)
        }
    }
}

/// Open a fresh sibling temp file with O_EXCL semantics (`create_new`):
/// never follows a pre-planted symlink — a planted path fails creation
/// instead of being written through (audit finding L2). Retries with a new
/// name on the (tampering-only) collision case.
fn create_new_temp(base: &Path) -> io::Result<(std::fs::File, PathBuf)> {
    for _ in 0..16 {
        let tmp = tmp_sibling(base);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(file) => return Ok((file, tmp)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique temp file",
    ))
}

/// Restrict a file to owner read/write only (`0600`) before data is written.
#[cfg(unix)]
fn set_private_mode(file: &std::fs::File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
}

/// Non-Unix targets: no-op (mode is platform-managed).
#[cfg(not(unix))]
fn set_private_mode(_file: &std::fs::File) -> io::Result<()> {
    Ok(())
}

/// Compute the SHA-256 of a file as a lowercase hex string.
pub(crate) async fn sha256_file_hex(path: &Path) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match file.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(err) => return Err(err),
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Copy `path` to `<path>.bak`, replacing any existing backup.
///
/// Security (audit finding L2): the copy goes through a fresh O_EXCL temp
/// file and a final `rename`, so a pre-planted symlink at `<path>.bak` is
/// atomically *replaced*, never followed (`std::fs::copy` would write
/// through the symlink). Source permissions are copied explicitly so a
/// `0600` kubeconfig never gains a world-readable backup.
pub(crate) fn backup(path: &Path) -> io::Result<()> {
    let mut bak = path.as_os_str().to_owned();
    bak.push(".bak");
    let bak = PathBuf::from(bak);
    let src = std::fs::File::open(path)?;
    let src_perms = src.metadata()?.permissions();
    let (mut dst, tmp) = create_new_temp(&bak)?;
    {
        let mut src = src;
        std::io::copy(&mut src, &mut dst)?;
    }
    dst.set_permissions(src_perms)?;
    dst.sync_all()?;
    drop(dst);
    match std::fs::rename(&tmp, &bak) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&tmp);
            Err(err)
        }
    }
}

/// Path of the backup file for `path` (used by tests and callers that
/// manage backups explicitly).
#[cfg(test)]
pub(crate) fn backup_path(path: &Path) -> PathBuf {
    let mut bak = path.as_os_str().to_owned();
    bak.push(".bak");
    PathBuf::from(bak)
}

/// Generate a unique temp-file path next to `path` (same directory, so the
/// final rename is atomic on the same filesystem).
fn tmp_sibling(path: &Path) -> PathBuf {
    let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(format!(".tmp-{}-{}", std::process::id(), counter));
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("kindboard-fsutil-test");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(format!(
            "{}-{}-{}.txt",
            tag,
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn writes_new_file() {
        let path = temp_file("new");
        let _ = std::fs::remove_file(&path);
        atomic_write(&path, b"hello").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
        assert!(!backup_path(&path).exists(), "no backup for new files");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn replaces_existing_file_and_backs_it_up() {
        let path = temp_file("existing");
        atomic_write(&path, b"v1").unwrap();
        atomic_write(&path, b"v2").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "v2");
        assert_eq!(std::fs::read_to_string(backup_path(&path)).unwrap(), "v1");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(backup_path(&path));
    }

    #[test]
    fn no_stray_tmp_files_left() {
        let path = temp_file("tmpcheck");
        atomic_write(&path, b"data").unwrap();
        atomic_write(&path, b"data2").unwrap();
        let dir = path.parent().unwrap();
        let stem = path.file_name().unwrap().to_string_lossy().to_string();
        let leftovers: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .filter(|n| {
                let name = n.to_string_lossy();
                name.contains(&stem) && name.contains(".tmp-")
            })
            .collect();
        assert!(leftovers.is_empty(), "stray tmp files: {leftovers:?}");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(backup_path(&path));
    }

    #[cfg(unix)]
    #[test]
    fn private_write_creates_0600() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_file("private");
        let _ = std::fs::remove_file(&path);
        atomic_write_private(&path, b"secret").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "private files must be owner-only");
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn backup_replaces_a_planted_symlink_instead_of_writing_through_it() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_file("symlink-def");
        let victim = temp_file("symlink-victim");
        atomic_write_private(&path, b"kubeconfig secret").unwrap();
        std::fs::write(&victim, b"unrelated victim content").unwrap();

        // Plant a symlink at `<path>.bak` pointing at the victim.
        let bak = backup_path(&path);
        let _ = std::fs::remove_file(&bak);
        std::os::unix::fs::symlink(&victim, &bak).unwrap();

        // A second write triggers the backup path; the symlink must be
        // replaced by a real copy and the victim must be untouched.
        atomic_write_private(&path, b"kubeconfig secret v2").unwrap();

        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "unrelated victim content",
            "backup must not write through a planted symlink"
        );
        let bak_meta = std::fs::symlink_metadata(&bak).unwrap();
        assert!(
            !bak_meta.file_type().is_symlink(),
            "backup must replace the symlink"
        );
        assert_eq!(
            std::fs::read_to_string(&bak).unwrap(),
            "kubeconfig secret",
            "backup must hold the previous content"
        );
        let bak_mode = bak_meta.permissions().mode() & 0o777;
        assert_eq!(
            bak_mode, 0o600,
            "a private file must not gain a world-readable backup"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&bak);
        let _ = std::fs::remove_file(&victim);
    }

    #[tokio::test]
    async fn sha256_file_matches_known_vector() {
        let path = temp_file("sha");
        atomic_write(&path, b"abc").unwrap();
        let hex = sha256_file_hex(&path).await.unwrap();
        assert_eq!(
            hex,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_file(&path);
    }
}
