use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Background watcher for a skill directory.
///
/// Uses the OS-native FS notification API (kqueue on macOS, inotify on Linux).
/// Callers snapshot `version()` at the start of each SM iteration and re-scan
/// the directory whenever the value has changed — no polling thread needed.
pub struct SkillWatcher {
    // Keep the watcher alive for the lifetime of this struct; dropping it
    // unregisters the OS watch.
    _watcher: RecommendedWatcher,
    version: Arc<AtomicU64>,
}

impl SkillWatcher {
    /// Start watching `dir`. Returns `None` if the path doesn't exist or the
    /// OS watcher cannot be created (e.g. unavailable on this platform).
    pub fn start(dir: &Path) -> Option<Self> {
        let version = Arc::new(AtomicU64::new(0));
        let version2 = Arc::clone(&version);

        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                let Ok(event) = res else { return };
                let is_relevant = matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ) && event.paths.iter().any(|p| {
                    matches!(
                        p.extension().and_then(|e| e.to_str()),
                        Some("md") | Some("json") | Some("py")
                    )
                });
                if is_relevant {
                    version2.fetch_add(1, Ordering::Relaxed);
                }
            })
            .ok()?;

        watcher.watch(dir, RecursiveMode::NonRecursive).ok()?;

        Some(Self {
            _watcher: watcher,
            version,
        })
    }

    /// Monotonically-increasing counter; increments whenever a `.md`, `.json`,
    /// or `.py` file in the watched directory is created, modified, or removed.
    ///
    /// Callers should snapshot this at the start of each loop iteration and
    /// compare against the previous snapshot; a change means the skill catalog
    /// should be refreshed.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }
}
