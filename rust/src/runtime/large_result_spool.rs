use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

/// Persist a spooled tool output under the host-selected directory and return the path ref.
pub fn persist_output(dir: &Path, call_id: &str, content: &str) -> std::io::Result<String> {
    std::fs::create_dir_all(dir)?;
    let filename = spool_filename(call_id, content);
    let path: PathBuf = dir.join(&filename);
    let temp_path = dir.join(format!(
        ".{filename}.tmp-{}-{}",
        std::process::id(),
        NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp_path, &path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result?;
    Ok(path.to_string_lossy().into_owned())
}

fn spool_filename(call_id: &str, content: &str) -> String {
    // Provider-issued call ids are untrusted and may contain separators. Hash the
    // identity instead of allowing it to participate in path construction.
    format!(
        "call-{:016x}-{:016x}.txt",
        simple_hash(call_id),
        simple_hash(content)
    )
}

fn simple_hash(content: &str) -> u64 {
    content.bytes().fold(0u64, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(u64::from(b))
    })
}

#[cfg(test)]
mod tests {
    use super::{persist_output, spool_filename};

    #[test]
    fn spool_filename_never_contains_untrusted_path_components() {
        let filename = spool_filename("../../outside/owned", "payload");
        assert!(!filename.contains('/'));
        assert!(!filename.contains(".."));
        assert!(filename.starts_with("call-"));
        assert!(filename.ends_with(".txt"));
    }

    #[test]
    fn persist_output_uses_host_selected_directory() {
        let dir = std::env::temp_dir().join(format!(
            "deepstrike-spool-test-{}-{}",
            std::process::id(),
            super::NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let reference = persist_output(&dir, "call/unsafe", "payload").expect("persist");
        let path = std::path::Path::new(&reference);
        assert_eq!(path.parent(), Some(dir.as_path()));
        assert_eq!(std::fs::read_to_string(path).expect("read"), "payload");
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
}
