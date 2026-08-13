//! # `aum-engine` — tasks, ingest, and the live view
//!
//! Ties the pieces together: adapters produce signals, storage records them,
//! and this crate decides what a task currently looks like.

pub mod ingest;
pub mod metrics;

pub use ingest::{PassStats, WatchRoot, ingest_file, ingest_root};
pub use metrics::{Completeness, MetricsInput};
