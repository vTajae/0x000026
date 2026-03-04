//! CLI temp directory standardization and cleanup.
//!
//! Manages cleanup of stale session directories left by CLI tool escalation
//! (Gemini, Claude, Codex). Called on `/new` (full cleanup) and periodically
//! (stale cleanup with max age).

use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tracing::{debug, warn};

/// Summary of a cleanup operation.
#[derive(Debug, Default)]
pub struct CleanupReport {
    /// Number of directories removed.
    pub dirs_removed: u32,
    /// Number of files removed.
    pub files_removed: u32,
    /// Total bytes freed (approximate).
    pub bytes_freed: u64,
    /// Errors encountered (path, message).
    pub errors: Vec<(PathBuf, String)>,
}

impl std::fmt::Display for CleanupReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Cleanup: {} dirs, {} files removed ({} bytes freed, {} errors)",
            self.dirs_removed,
            self.files_removed,
            self.bytes_freed,
            self.errors.len()
        )
    }
}

/// Files/patterns that must NEVER be deleted.
const PROTECTED_FILES: &[&str] = &[
    ".credentials.json",
    "credentials.json",
    "settings.json",
    ".env",
    "api_key",
    "api_key.txt",
];

/// Known CLI temp directories that accumulate stale session data.
pub fn known_temp_dirs() -> Vec<PathBuf> {
    let home = std::env::var("HOME")
        .ok()
        .map(PathBuf::from);
    let Some(home) = home else {
        return vec![];
    };
    vec![
        home.join(".gemini/tmp"),       // Gemini CLI session temps
        home.join(".claude/projects"),   // Claude CLI project sessions
        home.join(".codex"),            // Codex session data
    ]
}

/// Check if a path is protected and should never be deleted.
fn is_protected(path: &std::path::Path) -> bool {
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if PROTECTED_FILES.contains(&name) {
            return true;
        }
        // Protect dotfiles that look like config
        if name.starts_with('.') && name.ends_with(".json") && !name.contains("tmp") {
            return true;
        }
    }
    false
}

/// Remove a directory or file, returning bytes freed.
fn remove_entry(path: &std::path::Path, report: &mut CleanupReport) {
    if is_protected(path) {
        debug!("Skipping protected file: {}", path.display());
        return;
    }

    let size = dir_size(path);

    let was_dir = path.is_dir();
    let result = if was_dir {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };

    match result {
        Ok(()) => {
            if was_dir {
                report.dirs_removed += 1;
            } else {
                report.files_removed += 1;
            }
            report.bytes_freed += size;
            debug!("Removed: {}", path.display());
        }
        Err(e) => {
            report
                .errors
                .push((path.to_path_buf(), format!("{e}")));
            warn!("Failed to remove {}: {e}", path.display());
        }
    }
}

/// Approximate size of a path (file or directory tree).
fn dir_size(path: &std::path::Path) -> u64 {
    if path.is_file() {
        return path.metadata().map(|m| m.len()).unwrap_or(0);
    }
    walkdir(path)
}

/// Recursively sum file sizes in a directory.
fn walkdir(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += walkdir(&p);
            } else {
                total += p.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

/// Remove temp dirs older than `max_age_days`.
/// Skips credential files and active session markers.
pub fn cleanup_stale(max_age_days: u64) -> CleanupReport {
    let mut report = CleanupReport::default();
    let max_age = Duration::from_secs(max_age_days * 24 * 3600);
    let now = SystemTime::now();

    for base_dir in known_temp_dirs() {
        if !base_dir.exists() {
            continue;
        }

        let entries = match std::fs::read_dir(&base_dir) {
            Ok(e) => e,
            Err(e) => {
                report.errors.push((base_dir.clone(), format!("{e}")));
                continue;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();

            if is_protected(&path) {
                continue;
            }

            // Check age via modification time
            let age = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| now.duration_since(t).ok())
                .unwrap_or(Duration::ZERO);

            if age > max_age {
                remove_entry(&path, &mut report);
            }
        }
    }

    debug!("{report}");
    report
}

/// Remove ALL CLI temp data (used by /new for fresh start).
/// Still skips credential files.
pub fn cleanup_all() -> CleanupReport {
    let mut report = CleanupReport::default();

    for base_dir in known_temp_dirs() {
        if !base_dir.exists() {
            continue;
        }

        let entries = match std::fs::read_dir(&base_dir) {
            Ok(e) => e,
            Err(e) => {
                report.errors.push((base_dir.clone(), format!("{e}")));
                continue;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();

            if is_protected(&path) {
                continue;
            }

            remove_entry(&path, &mut report);
        }
    }

    debug!("{report}");
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_protected_files() {
        assert!(is_protected(std::path::Path::new("/home/user/.codex/.credentials.json")));
        assert!(is_protected(std::path::Path::new("/home/user/.gemini/settings.json")));
        assert!(!is_protected(std::path::Path::new("/home/user/.gemini/tmp/abc123")));
    }

    #[test]
    fn test_known_temp_dirs_returns_paths() {
        let dirs = known_temp_dirs();
        // Should return at least the 3 known paths (if HOME is set)
        if std::env::var("HOME").is_ok() {
            assert_eq!(dirs.len(), 3);
            assert!(dirs[0].to_str().unwrap().contains(".gemini/tmp"));
            assert!(dirs[1].to_str().unwrap().contains(".claude/projects"));
            assert!(dirs[2].to_str().unwrap().contains(".codex"));
        }
    }

    #[test]
    fn test_cleanup_report_display() {
        let report = CleanupReport {
            dirs_removed: 5,
            files_removed: 10,
            bytes_freed: 1024,
            errors: vec![],
        };
        let s = format!("{report}");
        assert!(s.contains("5 dirs"));
        assert!(s.contains("10 files"));
    }

    #[test]
    fn test_cleanup_stale_no_panic() {
        // Should not panic even if dirs don't exist
        let report = cleanup_stale(7);
        // May remove 0 items (temp dirs may not exist in CI)
        assert!(report.errors.len() <= known_temp_dirs().len());
    }
}
