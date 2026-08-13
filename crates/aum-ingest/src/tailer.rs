//! Reading append-only JSONL files safely.
//!
//! The agents write their transcripts by opening, appending, and closing — no
//! persistent file handle, so a size delta is a valid change signal. What makes
//! this non-trivial is the shape of the real data:
//!
//! * A line may be **half-written** when we read it. The persisted cursor is
//!   therefore advanced only past the last complete newline; the trailing
//!   fragment lives in memory and is never stored, so a restart simply re-reads
//!   it from disk.
//! * A single line can be **enormous**. The largest in the local corpus is
//!   **69.94 MiB**, in a file whose useful content is a few hundred kilobytes.
//!   Reading lines into a `String` would allocate that, repeatedly.
//! * Almost nothing is relevant. Only **0.09%** of Codex rollout bytes carry
//!   usage, so a byte-level prefilter runs before any JSON parsing.
//! * Files get **truncated or replaced**. Identity is `(device, inode)` rather
//!   than path, and a shrinking file or a changed head means start over.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Longest line we will materialize. Anything larger is skipped without being
/// held in memory; the relevant lines in both formats are around a kilobyte, so
/// this is generous by three orders of magnitude.
pub const MAX_LINE: usize = 8 * 1024 * 1024;

/// Read granularity.
const CHUNK: usize = 1024 * 1024;

/// How much of a line the prefilter sees. A discriminating key like
/// `"type":"assistant"` appears near the start; scanning megabytes of tool
/// output to find it would defeat the purpose.
const PREFILTER_WINDOW: usize = 4096;

/// Where reading of one file has got to.
///
/// `byte_offset` is always positioned just past a newline, which is the whole
/// safety property: it can never point into the middle of a record.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cursor {
    pub byte_offset: u64,
    pub line_ordinal: u64,
    pub size_seen: u64,
}

/// Stable file identity across renames and directory moves.
///
/// Path is deliberately not part of it. Claude Code's project directories are
/// derived from the working directory with separators replaced by dashes, which
/// is lossy — a literal `-` in a path is indistinguishable from a `/` — so a
/// path is a label, not an identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId {
    pub device: u64,
    pub inode: u64,
}

impl FileId {
    pub fn of(path: &Path) -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let m = std::fs::metadata(path)?;
            Ok(Self {
                device: m.dev(),
                inode: m.ino(),
            })
        }
        #[cfg(not(unix))]
        {
            // No inode concept; fall back to a hash of the canonical path.
            use std::hash::{Hash as _, Hasher as _};
            let canonical = std::fs::canonicalize(path)?;
            let mut h = std::collections::hash_map::DefaultHasher::new();
            canonical.hash(&mut h);
            Ok(Self {
                device: 0,
                inode: h.finish(),
            })
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadStats {
    /// Lines that passed the prefilter and were handed to the callback.
    pub delivered: u64,
    /// Complete lines seen, including ones the prefilter rejected.
    pub scanned: u64,
    /// Lines exceeding [`MAX_LINE`], skipped without being materialized.
    pub oversize_skipped: u64,
    /// Oversize lines that *would* have been relevant. Non-zero here means real
    /// data was dropped, and must be surfaced as an anomaly rather than ignored.
    pub oversize_relevant: u64,
    /// The file shrank or was replaced, so reading restarted from the beginning.
    pub restarted: bool,
}

/// A line handed to the consumer.
pub struct Line<'a> {
    pub ordinal: u64,
    /// Byte offset of the line's first byte.
    pub offset: u64,
    pub bytes: &'a [u8],
}

/// Read everything appended since `cursor`, calling `on_line` for each complete
/// line that passes `is_candidate`.
///
/// Returns the updated cursor. On any error the cursor is not advanced, so the
/// next attempt re-reads rather than skipping data.
pub fn read_new_lines<P, F>(
    path: &Path,
    cursor: Cursor,
    mut is_candidate: P,
    mut on_line: F,
) -> std::io::Result<(Cursor, ReadStats)>
where
    P: FnMut(&[u8]) -> bool,
    F: FnMut(Line<'_>),
{
    let mut stats = ReadStats::default();
    let metadata = std::fs::metadata(path)?;
    let size = metadata.len();

    let mut cursor = cursor;

    // A file that shrank was truncated or replaced. Re-reading it is safe
    // precisely because the dedup keys make ingest idempotent — which is why
    // that property was worth paying for.
    if size < cursor.byte_offset {
        cursor = Cursor::default();
        stats.restarted = true;
    }

    if size == cursor.byte_offset {
        cursor.size_seen = size;
        return Ok((cursor, stats));
    }

    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(cursor.byte_offset))?;

    let mut chunk = vec![0_u8; CHUNK];
    // Holds the current incomplete line. Bounded by MAX_LINE; never persisted.
    let mut pending: Vec<u8> = Vec::with_capacity(8 * 1024);
    let mut pending_start = cursor.byte_offset;
    let mut position = cursor.byte_offset;
    // Set when the current line has outgrown MAX_LINE: we stop accumulating and
    // scan for the terminating newline without holding the bytes.
    let mut skipping = false;
    let mut skipped_was_relevant = false;

    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        let mut rest = chunk.get(..read).unwrap_or_default();

        while let Some(nl) = memchr::memchr(b'\n', rest) {
            let (head, tail) = rest.split_at(nl);
            // `nl` is in bounds by construction, so this cannot underflow.
            rest = tail.get(1..).unwrap_or_default();

            stats.scanned = stats.scanned.saturating_add(1);
            cursor.line_ordinal = cursor.line_ordinal.saturating_add(1);

            if skipping {
                // The terminating newline of a line we already gave up on.
                stats.oversize_skipped = stats.oversize_skipped.saturating_add(1);
                if skipped_was_relevant {
                    stats.oversize_relevant = stats.oversize_relevant.saturating_add(1);
                }
                skipping = false;
                skipped_was_relevant = false;
                pending.clear();
            } else if pending.len().saturating_add(head.len()) > MAX_LINE {
                // A line can also cross the limit on the chunk that completes
                // it, not only on a chunk that ends mid-line. Checking in one
                // place only would let an oversize line through whenever its
                // final chunk happened to contain the newline.
                stats.oversize_skipped = stats.oversize_skipped.saturating_add(1);
                let window = prefilter_window(&pending, head);
                if is_candidate(window) {
                    stats.oversize_relevant = stats.oversize_relevant.saturating_add(1);
                }
                pending.clear();
                pending.shrink_to_fit();
            } else {
                pending.extend_from_slice(head);

                let window = pending
                    .get(..PREFILTER_WINDOW.min(pending.len()))
                    .unwrap_or_default();
                if is_candidate(window) {
                    stats.delivered = stats.delivered.saturating_add(1);
                    on_line(Line {
                        ordinal: cursor.line_ordinal,
                        offset: pending_start,
                        bytes: &pending,
                    });
                }
                pending.clear();
            }

            position = position
                .saturating_add(u64::try_from(nl).unwrap_or(0))
                .saturating_add(1);
            pending_start = position;

            // The cursor only ever moves to just past a newline. This is what
            // makes a half-written trailing line harmless.
            cursor.byte_offset = position;
        }

        // Whatever is left has no newline yet: an incomplete line.
        if !rest.is_empty() {
            if skipping {
                // Already over the limit; note whether it mattered, discard.
                position = position.saturating_add(u64::try_from(rest.len()).unwrap_or(0));
            } else if pending.len().saturating_add(rest.len()) > MAX_LINE {
                // Decide relevance from what we have before throwing it away, so
                // the anomaly can say whether real data was lost.
                skipped_was_relevant = is_candidate(prefilter_window(&pending, rest));
                skipping = true;
                pending.clear();
                pending.shrink_to_fit();
                position = position.saturating_add(u64::try_from(rest.len()).unwrap_or(0));
            } else {
                pending.extend_from_slice(rest);
                position = position.saturating_add(u64::try_from(rest.len()).unwrap_or(0));
            }
        }
    }

    cursor.size_seen = size;
    Ok((cursor, stats))
}

/// The bytes the prefilter judges a line by.
///
/// Normally the start of the accumulated line; when nothing has accumulated yet
/// (an oversize line arriving whole in one chunk) the start of the incoming
/// bytes instead, so relevance can still be determined before discarding it.
fn prefilter_window<'a>(pending: &'a [u8], incoming: &'a [u8]) -> &'a [u8] {
    let source = if pending.is_empty() {
        incoming
    } else {
        pending
    };
    source
        .get(..PREFILTER_WINDOW.min(source.len()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::io::Write as _;

    fn write(path: &Path, contents: &str) {
        let mut f = File::create(path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    fn append(path: &Path, contents: &str) {
        let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    fn collect(path: &Path, cursor: Cursor) -> (Cursor, ReadStats, Vec<String>) {
        let mut out = Vec::new();
        let (c, s) = read_new_lines(
            path,
            cursor,
            |_| true,
            |line| out.push(String::from_utf8_lossy(line.bytes).into_owned()),
        )
        .unwrap();
        (c, s, out)
    }

    #[test]
    fn reads_complete_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        write(&p, "{\"a\":1}\n{\"a\":2}\n");

        let (cursor, stats, lines) = collect(&p, Cursor::default());
        assert_eq!(lines, vec!["{\"a\":1}", "{\"a\":2}"]);
        assert_eq!(stats.delivered, 2);
        assert_eq!(cursor.byte_offset, 16);
        assert_eq!(cursor.line_ordinal, 2);
    }

    #[test]
    fn a_half_written_line_is_not_consumed_until_it_is_complete() {
        // The core safety property. A partially flushed record must not be
        // parsed, and must not be skipped either.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        write(&p, "{\"a\":1}\n{\"a\":2");

        let (cursor, _, lines) = collect(&p, Cursor::default());
        assert_eq!(lines, vec!["{\"a\":1}"]);
        // Positioned after the first newline, not into the fragment.
        assert_eq!(cursor.byte_offset, 8);

        append(&p, "}\n");
        let (cursor2, _, lines2) = collect(&p, cursor);
        assert_eq!(lines2, vec!["{\"a\":2}"]);
        assert_eq!(cursor2.byte_offset, 16);
    }

    #[test]
    fn resuming_from_a_cursor_does_not_re_deliver_earlier_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        write(&p, "one\ntwo\n");

        let (cursor, _, first) = collect(&p, Cursor::default());
        assert_eq!(first.len(), 2);

        append(&p, "three\n");
        let (_, _, second) = collect(&p, cursor);
        assert_eq!(second, vec!["three"]);
    }

    #[test]
    fn a_truncated_file_is_re_read_from_the_start() {
        // Safe only because the dedup keys make re-ingest idempotent.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        write(&p, "aaa\nbbb\nccc\n");
        let (cursor, _, _) = collect(&p, Cursor::default());
        assert_eq!(cursor.byte_offset, 12);

        write(&p, "x\n");
        let (cursor2, stats, lines) = collect(&p, cursor);
        assert!(stats.restarted);
        assert_eq!(lines, vec!["x"]);
        assert_eq!(cursor2.byte_offset, 2);
    }

    #[test]
    fn nothing_new_means_no_work() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        write(&p, "a\n");
        let (cursor, _, _) = collect(&p, Cursor::default());
        let (cursor2, stats, lines) = collect(&p, cursor);
        assert_eq!(cursor2, cursor);
        assert_eq!(stats.delivered, 0);
        assert!(lines.is_empty());
    }

    #[test]
    fn the_prefilter_runs_before_anything_expensive() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        write(
            &p,
            "{\"type\":\"assistant\"}\n{\"type\":\"user\"}\n{\"type\":\"assistant\"}\n",
        );

        let mut delivered = Vec::new();
        let (_, stats) = read_new_lines(
            &p,
            Cursor::default(),
            |head| memchr::memmem::find(head, b"\"type\":\"assistant\"").is_some(),
            |line| delivered.push(String::from_utf8_lossy(line.bytes).into_owned()),
        )
        .unwrap();

        assert_eq!(stats.scanned, 3);
        assert_eq!(stats.delivered, 2);
        assert_eq!(delivered.len(), 2);
    }

    #[test]
    fn an_oversize_line_is_skipped_without_being_held_in_memory() {
        // The 69.94 MiB line in the real corpus. Reading it into a String
        // allocates 70 MiB; here it must be stepped over.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");

        let mut contents = String::from("small-before\n");
        contents.push_str(&"x".repeat(MAX_LINE + 1_000));
        contents.push('\n');
        contents.push_str("small-after\n");
        write(&p, &contents);

        let (cursor, stats, lines) = collect(&p, Cursor::default());
        assert_eq!(lines, vec!["small-before", "small-after"]);
        assert_eq!(stats.oversize_skipped, 1);
        // Reading continued correctly past the giant line.
        assert_eq!(cursor.byte_offset, contents.len() as u64);
    }

    #[test]
    fn an_oversize_line_that_looked_relevant_is_reported_as_data_loss() {
        // Silently dropping a relevant record would understate a total with no
        // symptom at all. It has to be visible.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");

        let mut contents = String::from("{\"type\":\"assistant\",\"usage\":{");
        contents.push_str(&"x".repeat(MAX_LINE + 1_000));
        contents.push('\n');
        write(&p, &contents);

        let (_, stats) = read_new_lines(
            &p,
            Cursor::default(),
            |head| memchr::memmem::find(head, b"\"type\":\"assistant\"").is_some(),
            |_| {},
        )
        .unwrap();

        assert_eq!(stats.oversize_skipped, 1);
        assert_eq!(stats.oversize_relevant, 1);
    }

    #[test]
    fn a_line_spanning_many_read_chunks_is_reassembled_whole() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        let long = "y".repeat(CHUNK * 2 + 123);
        write(&p, &format!("{long}\n"));

        let (_, stats, lines) = collect(&p, Cursor::default());
        assert_eq!(stats.delivered, 1);
        assert_eq!(lines.first().map(String::len), Some(long.len()));
    }

    #[test]
    fn line_offsets_point_at_the_start_of_each_line() {
        // Offsets are the audit trail: they must locate the exact bytes a
        // number came from.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        write(&p, "aaaa\nbb\ncccccc\n");

        let mut offsets = Vec::new();
        read_new_lines(&p, Cursor::default(), |_| true, |l| offsets.push(l.offset)).unwrap();
        assert_eq!(offsets, vec![0, 5, 8]);
    }

    #[test]
    fn reading_the_same_content_twice_from_scratch_is_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        write(&p, "a\nb\nc\n");
        let first = collect(&p, Cursor::default());
        let second = collect(&p, Cursor::default());
        assert_eq!(first.0, second.0);
        assert_eq!(first.2, second.2);
    }

    #[test]
    fn a_file_with_no_trailing_newline_yields_nothing_until_it_gets_one() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        write(&p, "incomplete");
        let (cursor, stats, lines) = collect(&p, Cursor::default());
        assert!(lines.is_empty());
        assert_eq!(stats.delivered, 0);
        assert_eq!(cursor.byte_offset, 0);
    }

    #[test]
    fn arbitrary_split_points_yield_the_same_result_as_one_pass() {
        // The streaming property: processing a file in pieces must equal
        // processing it whole, for every split point. This is what makes a
        // restart mid-file safe.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        let full = "alpha\nbeta\ngamma\ndelta\nepsilon\n";

        let whole = {
            write(&p, full);
            collect(&p, Cursor::default()).2
        };

        for split in 1..full.len() {
            write(&p, full.get(..split).unwrap());
            let (cursor, _, mut lines) = collect(&p, Cursor::default());
            append(&p, full.get(split..).unwrap());
            let (_, _, rest) = collect(&p, cursor);
            lines.extend(rest);
            assert_eq!(lines, whole, "split at byte {split} diverged");
        }
    }
}
