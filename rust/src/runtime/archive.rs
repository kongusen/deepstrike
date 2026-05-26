use deepstrike_core::types::message::Message;
use std::error::Error;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

pub trait ArchiveStore: Send + Sync {
    fn write(
        &self,
        session_id: &str,
        seq: u64,
        messages: &[Message],
    ) -> Result<String, Box<dyn Error + Send + Sync>>;
}

pub struct NullArchiveStore;

impl ArchiveStore for NullArchiveStore {
    fn write(
        &self,
        _session_id: &str,
        _seq: u64,
        _messages: &[Message],
    ) -> Result<String, Box<dyn Error + Send + Sync>> {
        Ok(String::new())
    }
}

pub struct FileArchiveStore {
    pub root: PathBuf,
}

impl FileArchiveStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl ArchiveStore for FileArchiveStore {
    fn write(
        &self,
        session_id: &str,
        seq: u64,
        messages: &[Message],
    ) -> Result<String, Box<dyn Error + Send + Sync>> {
        let dir = self.root.join(session_id);
        fs::create_dir_all(&dir)?;
        let file_path = dir.join(format!("{}.jsonl", seq));
        let mut file = File::create(&file_path)?;

        for msg in messages {
            let line = serde_json::to_string(msg)?;
            writeln!(file, "{}", line)?;
        }

        Ok(file_path.to_string_lossy().to_string())
    }
}
