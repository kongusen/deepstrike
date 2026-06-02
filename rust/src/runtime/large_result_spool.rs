use std::path::{Path, PathBuf};

/// Persist a spooled tool output to `.spool/` and return the on-disk path ref.
pub fn persist_output(call_id: &str, content: &str) -> std::io::Result<String> {
    let dir = Path::new(".spool");
    std::fs::create_dir_all(dir)?;
    let digest = format!("{:x}", simple_hash(content));
    let path: PathBuf = dir.join(format!("{call_id}-{}.txt", &digest[..16.min(digest.len())]));
    std::fs::write(&path, content)?;
    Ok(path.to_string_lossy().into_owned())
}

fn simple_hash(content: &str) -> u64 {
    content.bytes().fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(u64::from(b)))
}
