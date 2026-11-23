//! Private preparation results. Lock files are stable: replacing an entry must
//! never replace the inode on which another process is waiting.
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn file_digest(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub fn root() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("PAPERBOY_CACHE_DIR") {
        if path.is_empty() {
            return Err("PAPERBOY_CACHE_DIR must not be empty".into());
        }
        return Ok(PathBuf::from(path));
    }
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|p| PathBuf::from(p).join("Library/Caches"));
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".cache")));
    base.map(|p| p.join("paperboy/trail"))
        .ok_or_else(|| "cannot locate the user cache directory; set PAPERBOY_CACHE_DIR".into())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub schema: u32,
    pub generation: String,
    pub updated_at: u64,
    pub values: Option<BTreeMap<String, String>>,
}

impl Entry {
    pub fn new(values: Option<BTreeMap<String, String>>) -> Self {
        Self {
            schema: 1,
            generation: uuid::Uuid::new_v4().to_string(),
            updated_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock predates Unix epoch")
                .as_secs(),
            values,
        }
    }

    pub fn validate(&self, names: &[String]) -> Result<(), String> {
        if self.schema != 1 || uuid::Uuid::parse_str(&self.generation).is_err() {
            return Err("unsupported or malformed cache entry".into());
        }
        if let Some(values) = &self.values
            && (values.len() != names.len()
                || names.iter().any(|n| {
                    values
                        .get(n)
                        .is_none_or(|v| v.trim().is_empty() || v.contains("{{"))
                }))
        {
            return Err("cache entry has missing, empty, or unresolved persisted values".into());
        }
        Ok(())
    }
}

pub struct LockedEntry {
    path: PathBuf,
    _lock: File,
}

fn private_dir(path: &Path) -> Result<(), String> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(path)
            .map_err(|e| e.to_string())?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err(format!(
                "cache directory {} must be owner-only (0700)",
                path.display()
            ));
        }
    }
    Ok(())
}

fn private_file(path: &Path, new: bool) -> Result<File, String> {
    let mut opts = OpenOptions::new();
    opts.read(true).write(true);
    if new {
        opts.create_new(true);
    } else {
        opts.create(true).truncate(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let f = opts
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if f.metadata()
            .map_err(|e| e.to_string())?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err(format!(
                "cache file {} must be owner-only (0600)",
                path.display()
            ));
        }
    }
    Ok(f)
}

impl LockedEntry {
    pub fn acquire(path: &Path, stopped: impl Fn() -> bool) -> Result<Self, String> {
        private_dir(path.parent().ok_or("cache entry has no parent")?)?;
        let lock = private_file(&path.with_extension("lock"), false)?;
        loop {
            if stopped() {
                return Err("cache operation interrupted".into());
            }
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(e.to_string()),
            }
        }
        Ok(Self {
            path: path.to_path_buf(),
            _lock: lock,
        })
    }

    pub fn read(&self, names: &[String]) -> Result<Option<Entry>, String> {
        let file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if file
                .metadata()
                .map_err(|e| e.to_string())?
                .permissions()
                .mode()
                & 0o077
                != 0
            {
                return Err("cache entry must be owner-only (0600)".into());
            }
        }
        let entry: Entry = serde_json::from_reader(file)
            .map_err(|e| format!("invalid cache entry {}: {e}", self.path.display()))?;
        entry.validate(names)?;
        Ok(Some(entry))
    }

    pub fn write(&self, entry: &Entry) -> Result<(), String> {
        let temp = self
            .path
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = private_file(&temp, true)?;
            serde_json::to_writer(&mut file, entry).map_err(|e| e.to_string())?;
            file.write_all(b"\n").map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            drop(file);
            fs::rename(&temp, &self.path).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            File::open(self.path.parent().unwrap())
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
            Ok(())
        })();
        if result.is_err()
            && let Err(e) = fs::remove_file(&temp)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return Err(format!(
                "{}; removing temporary cache entry: {e}",
                result.unwrap_err()
            ));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_store_roundtrip_invalidation_and_corruption() {
        let dir = std::env::temp_dir().join(format!("paperboy-cache-{}", uuid::Uuid::new_v4()));
        let path = dir.join("entry.json");
        {
            let entry = LockedEntry::acquire(&path, || false).unwrap();
            assert!(entry.read(&["session".into()]).unwrap().is_none());
            entry
                .write(&Entry::new(Some(BTreeMap::from([(
                    "session".into(),
                    "one".into(),
                )]))))
                .unwrap();
            assert_eq!(
                entry
                    .read(&["session".into()])
                    .unwrap()
                    .unwrap()
                    .values
                    .unwrap()["session"],
                "one"
            );
            assert!(entry.read(&["other".into()]).is_err());
            entry.write(&Entry::new(None)).unwrap();
            assert!(
                entry
                    .read(&["session".into()])
                    .unwrap()
                    .unwrap()
                    .values
                    .is_none()
            );
            fs::write(&path, b"broken").unwrap();
            assert!(entry.read(&["session".into()]).is_err());
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cache_lock_wait_can_be_cancelled() {
        let dir = std::env::temp_dir().join(format!("paperboy-cache-{}", uuid::Uuid::new_v4()));
        let path = dir.join("entry.json");
        let lock = LockedEntry::acquire(&path, || false).unwrap();
        assert!(LockedEntry::acquire(&path, || true).is_err());
        drop(lock);
        assert!(LockedEntry::acquire(&path, || false).is_ok());
        fs::remove_dir_all(dir).unwrap();
    }
}
