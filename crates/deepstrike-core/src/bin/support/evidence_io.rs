//! Shared evidence-plane readers for the host-side CLIs.

use std::path::{Path, PathBuf};

pub fn read_journal(path: &Path, blobs: &mut Vec<Vec<u8>>) -> Result<usize, String> {
    let before = blobs.len();
    if path.is_dir() {
        let mut entries = json_entries(path, false)?;
        entries.retain(|entry| {
            entry
                .extension()
                .is_some_and(|extension| extension == "json")
        });
        for entry in entries {
            blobs.push(read_file(&entry)?);
        }
        return Ok(blobs.len() - before);
    }
    split_blob_file(&read_file(path)?, blobs);
    Ok(blobs.len() - before)
}

pub fn read_session_log(path: &Path, streams: &mut Vec<Vec<Vec<u8>>>) -> Result<usize, String> {
    let mut added = 0usize;
    if path.is_dir() {
        for entry in json_entries(path, true)? {
            let mut stream = Vec::new();
            split_blob_file(&read_file(&entry)?, &mut stream);
            added += stream.len();
            streams.push(stream);
        }
        return Ok(added);
    }
    let mut stream = Vec::new();
    split_blob_file(&read_file(path)?, &mut stream);
    added += stream.len();
    streams.push(stream);
    Ok(added)
}

pub fn read_checkpoint(path: &Path, blobs: &mut Vec<Vec<u8>>) -> Result<usize, String> {
    let before = blobs.len();
    read_journal(path, blobs)?;
    for blob in &mut blobs[before..] {
        if let Ok(serde_json::Value::Object(map)) = serde_json::from_slice(blob)
            && let Some(nested) = map.get("checkpoint")
            && nested.is_object()
        {
            *blob = serde_json::to_vec(nested).unwrap_or_default();
        }
    }
    Ok(blobs.len() - before)
}

fn json_entries(path: &Path, include_jsonl: bool) -> Result<Vec<PathBuf>, String> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(path)
        .map_err(|error| format!("cannot read directory {}: {error}", path.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|entry| {
            entry.extension().is_some_and(|extension| {
                extension == "json" || (include_jsonl && extension == "jsonl")
            })
        })
        .collect();
    entries.sort();
    Ok(entries)
}

fn read_file(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))
}

/// A JSON array yields one blob per element, a single JSON value yields one blob, and anything
/// else is treated as JSON Lines. Array elements are compacted because the validator checks the
/// decoded record's own digest, not the source file's whitespace.
pub fn split_blob_file(bytes: &[u8], blobs: &mut Vec<Vec<u8>>) {
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(serde_json::Value::Array(records)) => {
            for record in records {
                blobs.push(serde_json::to_vec(&record).unwrap_or_default());
            }
        }
        Ok(_) => blobs.push(bytes.to_vec()),
        Err(_) => {
            for line in bytes.split(|byte| *byte == b'\n') {
                let line = trim_ascii(line);
                if !line.is_empty() {
                    blobs.push(line.to_vec());
                }
            }
        }
    }
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |position| position + 1);
    &bytes[start..end]
}
