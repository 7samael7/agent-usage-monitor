//! # `aum-domain` — the correctness core
//!
//! Pure. No I/O, no async, no database, no HTTP. That is deliberate: this crate
//! holds the rules that decide whether every number the application shows is
//! right, so it must be trivially reviewable and its tests must run instantly.
//!
//! The two things it exists for:
//!
//! * [`TokenUsage`] — the canonical, disjoint token partition, and the only
//!   place where Anthropic's and OpenAI's incompatible `input_tokens` semantics
//!   are reconciled. Private fields and provider-specific constructors make the
//!   double-counting bug unrepresentable rather than merely discouraged.
//! * [`native`] — the providers' own fields, kept verbatim as an audit trail, so
//!   any displayed number can be traced back to the bytes it came from.

pub mod native;
pub mod token_usage;

pub use native::{AnthropicUsage, NativeUsage, OpenAiUsage};
pub use token_usage::{NormalizeError, TokenUsage};
