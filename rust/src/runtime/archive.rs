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

    fn read(
        &self,
        archive_ref: &str,
    ) -> Result<Vec<Message>, Box<dyn Error + Send + Sync>>;
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

    fn read(
        &self,
        _archive_ref: &str,
    ) -> Result<Vec<Message>, Box<dyn Error + Send + Sync>> {
        Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "NullArchiveStore does not store archives",
        )))
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

    fn read(
        &self,
        archive_ref: &str,
    ) -> Result<Vec<Message>, Box<dyn Error + Send + Sync>> {
        let file = File::open(archive_ref)?;
        let reader = std::io::BufReader::new(file);
        let mut messages = Vec::new();
        use std::io::BufRead;
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let msg: Message = serde_json::from_str(&line)?;
            messages.push(msg);
        }
        Ok(messages)
    }
}
