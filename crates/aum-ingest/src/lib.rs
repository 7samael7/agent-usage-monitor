//! # `aum-ingest` — reading agent transcripts
//!
//! Both adapters need identical machinery: byte cursors that survive restarts,
//! partial-line handling, a cheap prefilter, and a change signal that does not
//! miss writes. That machinery is hard to get right and must exist exactly
//! once, so it lives here rather than in each adapter.

pub mod tailer;
pub mod watcher;

pub use tailer::{Cursor, FileId, Line, MAX_LINE, ReadStats, read_new_lines};
