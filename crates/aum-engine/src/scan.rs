//! Deciding which files are worth re-reading.
//!
//! The naive loop re-examines everything: a full recursive directory walk, then
//! a `stat`, a database write and a cursor read for every file it finds. On this
//! machine that is 522 files, and a live run did seventy passes in two and a
//! half minutes — roughly 36,000 file examinations and as many database writes
//! to discover that nothing had changed.
//!
//! Two caches fix it, both keyed on facts the filesystem already gives us:
//!
//! * **Directory listings** are re-walked only when a directory's own mtime
//!   moves, which is what changes when a file is added or removed.
//! * **Files** are re-read only when their size or mtime moves, which is what
//!   changes when an agent appends a line.
//!
//! Everything the caches decide is a *hint*. A file that is wrongly considered
//! unchanged is only ever late, never lost — the next real change picks it up,
//! cursors mean the read resumes exactly where it stopped, and dedup keys mean
//! a redundant read costs nothing but time.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// What we last saw of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seen {
    size: u64,
    modified: Option<SystemTime>,
}

impl Seen {
    fn of(metadata: &std::fs::Metadata) -> Self {
        Self {
            size: metadata.len(),
            modified: metadata.modified().ok(),
        }
    }
}

/// Remembers what has already been examined, so a pass costs nothing when
/// nothing has happened.
#[derive(Debug, Default)]
pub struct ScanCache {
    files: HashMap<PathBuf, Seen>,
    directories: HashMap<PathBuf, Option<SystemTime>>,
    /// Cached listing per root, rebuilt only when a directory changes.
    listings: HashMap<PathBuf, Vec<PathBuf>>,
}

/// What a scan decided.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanResult {
    /// Files that grew, shrank, or are new. These are worth reading.
    pub changed: Vec<PathBuf>,
    /// How many files were considered and skipped.
    pub unchanged: u32,
    /// True when the directory tree itself was re-walked.
    pub rewalked: bool,
}

impl ScanCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Files under `root` that have changed since the last call.
    ///
    /// `matches` decides which filenames are interesting; the caller supplies it
    /// so this stays independent of any adapter.
    pub fn changed_since_last<F>(&mut self, root: &Path, matches: &F) -> ScanResult
    where
        F: Fn(&Path) -> bool,
    {
        let mut result = ScanResult::default();

        if !root.is_dir() {
            // The agent is not installed, or has never run.
            self.listings.remove(root);
            return result;
        }

        // A directory's mtime moves when an entry is added or removed, so the
        // expensive recursive walk is only needed when one of them has.
        let listing = if self.tree_changed(root) {
            result.rewalked = true;
            let mut files = Vec::new();
            self.walk(root, matches, &mut files);
            files.sort_by_key(|p| {
                std::fs::metadata(p)
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH)
            });
            files.reverse();
            self.listings.insert(root.to_path_buf(), files.clone());
            files
        } else {
            self.listings.get(root).cloned().unwrap_or_default()
        };

        for path in listing {
            let Ok(metadata) = std::fs::metadata(&path) else {
                // Vanished between listing and stat. Forget it; if it returns,
                // the directory mtime will have moved.
                self.files.remove(&path);
                continue;
            };
            let now = Seen::of(&metadata);

            match self.files.get(&path) {
                Some(previous) if *previous == now => {
                    result.unchanged = result.unchanged.saturating_add(1);
                }
                _ => {
                    self.files.insert(path.clone(), now);
                    result.changed.push(path);
                }
            }
        }

        result
    }

    /// Did any directory in the tree change?
    ///
    /// Checks every directory's mtime, which is far cheaper than listing them:
    /// a `stat` per directory rather than a `readdir` per directory plus a
    /// `stat` per file.
    fn tree_changed(&mut self, root: &Path) -> bool {
        let mut directories = Vec::new();
        collect_dirs(root, &mut directories);

        // Record what we saw even on the very first call. Returning early here
        // would leave the directory mtimes unrecorded, so the *second* pass
        // would see them all as new and re-walk a tree that had not changed —
        // exactly the cost this cache exists to avoid.
        let mut changed = !self.listings.contains_key(root);
        for dir in &directories {
            let modified = std::fs::metadata(dir).and_then(|m| m.modified()).ok();
            match self.directories.get(dir) {
                Some(previous) if *previous == modified => {}
                _ => {
                    self.directories.insert(dir.clone(), modified);
                    changed = true;
                }
            }
        }

        // A directory disappearing also counts.
        if self.directories.len() != directories.len() {
            self.directories.retain(|d, _| directories.contains(d));
            changed = true;
        }

        changed
    }

    fn walk<F>(&self, dir: &Path, matches: &F, out: &mut Vec<PathBuf>)
    where
        F: Fn(&Path) -> bool,
    {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            if is_dir {
                self.walk(&path, matches, out);
            } else if matches(&path) {
                out.push(path);
            }
        }
    }

    /// Forget everything, forcing the next pass to re-examine.
    pub fn invalidate(&mut self) {
        self.files.clear();
        self.directories.clear();
        self.listings.clear();
    }

    #[must_use]
    pub fn known_files(&self) -> usize {
        self.files.len()
    }
}

fn collect_dirs(dir: &Path, out: &mut Vec<PathBuf>) {
    out.push(dir.to_path_buf());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            collect_dirs(&entry.path(), out);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::io::Write as _;

    fn jsonl(path: &Path) -> bool {
        path.extension().is_some_and(|e| e == "jsonl")
    }

    fn write(path: &Path, contents: &str) {
        std::fs::write(path, contents).unwrap();
    }

    fn append(path: &Path, contents: &str) {
        let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    /// Filesystem mtime resolution is coarse enough that two writes in the same
    /// instant can look identical. Real agents do not append at that rate, but
    /// tests do.
    fn settle() {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    #[test]
    fn the_first_pass_sees_everything() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a.jsonl"), "x\n");
        write(&dir.path().join("b.jsonl"), "y\n");

        let mut cache = ScanCache::new();
        let result = cache.changed_since_last(dir.path(), &jsonl);
        assert_eq!(result.changed.len(), 2);
        assert!(result.rewalked);
    }

    #[test]
    fn a_pass_over_an_idle_tree_finds_nothing_to_do() {
        // The bug this exists to fix: seventy passes doing full work each time.
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a.jsonl"), "x\n");

        let mut cache = ScanCache::new();
        cache.changed_since_last(dir.path(), &jsonl);

        let second = cache.changed_since_last(dir.path(), &jsonl);
        assert!(
            second.changed.is_empty(),
            "nothing changed, nothing to read"
        );
        assert_eq!(second.unchanged, 1);
        assert!(!second.rewalked, "an unchanged tree is not re-walked");
    }

    #[test]
    fn an_appended_file_is_picked_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        write(&path, "x\n");

        let mut cache = ScanCache::new();
        cache.changed_since_last(dir.path(), &jsonl);

        settle();
        append(&path, "y\n");

        let result = cache.changed_since_last(dir.path(), &jsonl);
        assert_eq!(result.changed, vec![path]);
    }

    #[test]
    fn a_new_file_is_picked_up_even_though_the_listing_was_cached() {
        // The directory's own mtime moves when an entry is added, which is what
        // makes caching the listing safe.
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a.jsonl"), "x\n");

        let mut cache = ScanCache::new();
        cache.changed_since_last(dir.path(), &jsonl);

        settle();
        write(&dir.path().join("b.jsonl"), "y\n");

        let result = cache.changed_since_last(dir.path(), &jsonl);
        assert!(result.rewalked, "adding a file must trigger a re-walk");
        assert_eq!(result.changed.len(), 1);
        assert!(
            result.changed.first().unwrap().ends_with("b.jsonl"),
            "only the new file needs reading"
        );
    }

    #[test]
    fn a_file_added_to_a_nested_directory_is_found() {
        // Claude Code's sub-agent transcripts live one level down and carry a
        // third of the usage, so a cache that only watched the root would lose
        // them.
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("session").join("subagents");
        std::fs::create_dir_all(&nested).unwrap();
        write(&dir.path().join("session.jsonl"), "x\n");

        let mut cache = ScanCache::new();
        cache.changed_since_last(dir.path(), &jsonl);

        settle();
        write(&nested.join("agent-1.jsonl"), "y\n");

        let result = cache.changed_since_last(dir.path(), &jsonl);
        assert_eq!(result.changed.len(), 1);
        assert!(result.changed.first().unwrap().ends_with("agent-1.jsonl"));
    }

    #[test]
    fn a_truncated_file_is_picked_up() {
        // Size shrinking is as much a change as size growing.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        write(&path, "aaaa\nbbbb\n");

        let mut cache = ScanCache::new();
        cache.changed_since_last(dir.path(), &jsonl);

        settle();
        write(&path, "c\n");

        assert_eq!(
            cache.changed_since_last(dir.path(), &jsonl).changed.len(),
            1
        );
    }

    #[test]
    fn a_deleted_file_stops_being_offered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        write(&path, "x\n");

        let mut cache = ScanCache::new();
        cache.changed_since_last(dir.path(), &jsonl);

        settle();
        std::fs::remove_file(&path).unwrap();

        let result = cache.changed_since_last(dir.path(), &jsonl);
        assert!(result.changed.is_empty());
    }

    #[test]
    fn files_that_do_not_match_are_never_offered() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a.jsonl"), "x\n");
        write(&dir.path().join("notes.txt"), "y\n");

        let mut cache = ScanCache::new();
        let result = cache.changed_since_last(dir.path(), &jsonl);
        assert_eq!(result.changed.len(), 1);
        assert!(result.changed.first().unwrap().ends_with("a.jsonl"));
    }

    #[test]
    fn a_missing_root_is_not_an_error() {
        // Neither agent installed. Must be quiet, not noisy.
        let mut cache = ScanCache::new();
        let result = cache.changed_since_last(Path::new("/definitely/not/here"), &jsonl);
        assert!(result.changed.is_empty());
        assert!(!result.rewalked);
    }

    #[test]
    fn invalidating_forces_a_full_re_examination() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a.jsonl"), "x\n");

        let mut cache = ScanCache::new();
        cache.changed_since_last(dir.path(), &jsonl);
        assert!(
            cache
                .changed_since_last(dir.path(), &jsonl)
                .changed
                .is_empty()
        );

        cache.invalidate();
        assert_eq!(
            cache.changed_since_last(dir.path(), &jsonl).changed.len(),
            1
        );
    }

    #[test]
    fn newest_files_are_offered_first() {
        // On first run a machine with months of history should show this week
        // immediately rather than spending its first minute on last spring.
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("old.jsonl"), "x\n");
        settle();
        write(&dir.path().join("new.jsonl"), "y\n");

        let mut cache = ScanCache::new();
        let result = cache.changed_since_last(dir.path(), &jsonl);
        assert!(
            result.changed.first().unwrap().ends_with("new.jsonl"),
            "got {:?}",
            result.changed
        );
    }
}
