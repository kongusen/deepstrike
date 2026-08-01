use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use deepstrike_core::runtime::kernel::wire::canonical_digest;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

pub trait PayloadStore: Send + Sync {
    fn persist(&self, session_id: &str, payload_ref: &str, content: &str) -> std::io::Result<()>;
    fn load(&self, session_id: &str, payload_ref: &str) -> std::io::Result<Option<String>>;
}

pub struct FilePayloadStore {
    root: PathBuf,
}

impl FilePayloadStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path(&self, session_id: &str, payload_ref: &str) -> PathBuf {
        let identity = format!("{session_id}\0{payload_ref}");
        let digest = canonical_digest(identity.as_bytes());
        self.root.join(format!(
            "{}.payload",
            digest.as_str().trim_start_matches("sha256:")
        ))
    }
}

impl PayloadStore for FilePayloadStore {
    fn persist(&self, session_id: &str, payload_ref: &str, content: &str) -> std::io::Result<()> {
        fs::create_dir_all(&self.root)?;
        let target = self.path(session_id, payload_ref);
        let temporary = temporary_path(&target);
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, target)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    fn load(&self, session_id: &str, payload_ref: &str) -> std::io::Result<Option<String>> {
        match fs::read_to_string(self.path(session_id, payload_ref)) {
            Ok(content) => Ok(Some(content)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

fn temporary_path(target: &Path) -> PathBuf {
    target.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

#[cfg(test)]
mod tests {
    use super::{FilePayloadStore, PayloadStore};
    use std::fs;

    #[test]
    fn opaque_locators_are_hashed_and_session_scoped() {
        let root = std::env::temp_dir().join(format!(
            "deepstrike-payload-test-{}-{}",
            std::process::id(),
            super::NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let store = FilePayloadStore::new(&root);
        store
            .persist("session-a", "../../payload", "alpha")
            .expect("persist a");
        store
            .persist("session-b", "../../payload", "beta")
            .expect("persist b");

        assert_eq!(
            store.load("session-a", "../../payload").expect("load a"),
            Some("alpha".to_string())
        );
        assert_eq!(
            store.load("session-b", "../../payload").expect("load b"),
            Some("beta".to_string())
        );
        assert_eq!(
            store.load("session-c", "../../payload").expect("load c"),
            None
        );
        assert_eq!(fs::read_dir(&root).expect("list").count(), 2);
        fs::remove_dir_all(root).expect("cleanup");
    }
}
