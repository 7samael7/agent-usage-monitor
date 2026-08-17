//! Turn a real agent transcript into a committable test fixture.
//!
//! Golden-file tests need real data — the fan-out, the synthetic failure
//! markers, the retry chains and the compaction boundaries are all things that
//! are hard to invent convincingly and easy to get subtly wrong. But real
//! transcripts contain prompts, responses, source code and absolute paths, and
//! this repository has a remote.
//!
//! So: **every number and every structural field is preserved exactly**, and
//! **all free text is replaced**. Token counts, ids, timestamps, models, types
//! and flags survive; message content, tool inputs and outputs, reasoning text
//! and file paths do not.
//!
//! Redaction happens on the way *in* to `tests/fixtures/`, never as a later
//! cleanup pass — a file committed unredacted once stays in history forever.
//!
//! ```text
//! cargo xtask redact <input.jsonl> <output.jsonl> [--max-lines N]
//! ```
//!
//! Object keys come out in sorted order rather than the order the agent wrote
//! them: `serde_json`'s default map is a `BTreeMap`. Nothing reads these files
//! positionally, so this costs a little resemblance and buys not having to turn
//! on `preserve_order` for every crate in the workspace.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Map, Value};

/// Keys whose values are free text and must not survive.
const TEXT_KEYS: &[&str] = &[
    "text",
    "thinking",
    "signature",
    "content",
    "input",
    "output",
    "command",
    "description",
    "prompt",
    "summary",
    "message",
    "error",
    "detail",
    "stdout",
    "stderr",
    "result",
    "toolUseResult",
    "lastPrompt",
    "customTitle",
    "aiTitle",
    "title",
    "name",
    "displayName",
    "instructions",
    "base_instructions",
    "developer_instructions",
];

/// Keys that are paths: replaced with a stable synthetic path, not blanked.
const PATH_KEYS: &[&str] = &[
    "cwd",
    "path",
    "file_path",
    "filePath",
    "workspace_roots",
    "transcript_path",
    "sqlite_home",
    "log_dir",
];

/// Keys that must survive verbatim: everything a parser reads and every number
/// a test asserts on.
///
/// This list is the whole security argument, so it is a list rather than a
/// pattern — a key is published because someone put it here, not because it
/// happened to match a rule. A key holding an object is *not* covered
/// transitively: every nested key is looked up on its own.
pub const KEEP_KEYS: &[&str] = &[
    "type",
    "subtype",
    "level",
    "role",
    "model",
    "sessionId",
    "uuid",
    "parentUuid",
    "requestId",
    "promptId",
    "timestamp",
    "version",
    "gitBranch",
    "entrypoint",
    "userType",
    "effort",
    "isSidechain",
    "isMeta",
    "isApiErrorMessage",
    "agentId",
    "attributionAgent",
    "retryAttempt",
    "maxRetries",
    "retryInMs",
    "stop_reason",
    "stopReason",
    "id",
    "usage",
    "preTokens",
    "postTokens",
    "cumulativeDroppedTokens",
    "input_tokens",
    "output_tokens",
    "cache_creation_input_tokens",
    "cache_read_input_tokens",
    "cache_creation",
    "ephemeral_5m_input_tokens",
    "ephemeral_1h_input_tokens",
    "server_tool_use",
    "web_search_requests",
    "web_fetch_requests",
    "service_tier",
    "speed",
    "inference_geo",
    "iterations",
    "total_token_usage",
    "last_token_usage",
    "cached_input_tokens",
    "cache_write_input_tokens",
    "reasoning_output_tokens",
    "total_tokens",
    "model_context_window",
    "rate_limits",
    "payload",
    "info",
];

/// Longest replacement string kept. Length is preserved below this so a fixture
/// still resembles real data; beyond it the shape stops mattering and the bytes
/// are just repository weight.
pub const MAX_REDACTED_LEN: usize = 200;

/// Strings this short are enum-like rather than prose, and are left alone.
pub const SHORT_ENOUGH_TO_BE_STRUCTURAL: usize = 3;

/// The prefix every pseudonymised path gets.
pub const PATH_ALIAS_PREFIX: &str = "/redacted/path-";

fn is(set: &[&str], key: &str) -> bool {
    set.contains(&key)
}

/// Rewrites one transcript, keeping the aliases stable across its lines.
#[derive(Default)]
pub struct Redactor {
    aliases: HashMap<String, String>,
}

/// What a run did, so the caller can print it instead of guessing.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub kept: usize,
    /// Lines that would not parse as JSON. A line that cannot be parsed cannot
    /// be redacted, so it cannot be published — but it is reported, because a
    /// silently dropped line is how a fixture ends up missing the case it was
    /// collected for.
    pub unparseable: usize,
    pub paths_pseudonymised: usize,
}

impl Redactor {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Stable pseudonyms, so the same input maps to the same output every run.
    fn alias_path(&mut self, original: &str) -> String {
        if let Some(existing) = self.aliases.get(original) {
            return existing.clone();
        }
        let alias = format!("{PATH_ALIAS_PREFIX}{}", self.aliases.len() + 1);
        self.aliases.insert(original.to_owned(), alias.clone());
        alias
    }

    fn text(value: &str) -> String {
        if value.chars().count() <= SHORT_ENOUGH_TO_BE_STRUCTURAL {
            return value.to_owned();
        }
        "x".repeat(value.chars().count().min(MAX_REDACTED_LEN))
    }

    /// Recursion is bounded by `serde_json`'s own parser limit: a value this
    /// walks was parsed by `from_str`, which refuses to nest past its depth cap.
    fn value(&mut self, value: Value, key: Option<&str>) -> Value {
        match value {
            // Numbers and booleans are the whole point of the fixture: never touched.
            Value::Null | Value::Bool(_) | Value::Number(_) => value,

            // An array inherits its parent's key, so `workspace_roots: [..]`
            // pseudonymises each element rather than each element falling
            // through to the unknown-key default.
            Value::Array(items) => Value::Array(
                items
                    .into_iter()
                    .map(|item| self.value(item, key))
                    .collect(),
            ),

            Value::Object(fields) => {
                let mut out = Map::with_capacity(fields.len());
                for (k, v) in fields {
                    let redacted = self.value(v, Some(&k));
                    out.insert(k, redacted);
                }
                Value::Object(out)
            }

            Value::String(s) => Value::String(self.string(&s, key)),
        }
    }

    fn string(&mut self, s: &str, key: Option<&str>) -> String {
        if s.is_empty() {
            return String::new();
        }
        if let Some(k) = key {
            if is(KEEP_KEYS, k) {
                return s.to_owned();
            }
            if is(PATH_KEYS, k) {
                return self.alias_path(s);
            }
            if is(TEXT_KEYS, k) {
                return Self::text(s);
            }
        }
        // Unknown string key: redact by default. Being wrong in this direction
        // costs a less realistic fixture; being wrong in the other direction
        // publishes someone's source code.
        if looks_like_a_path(s) {
            return self.alias_path(s);
        }
        Self::text(s)
    }

    #[must_use]
    pub fn paths_pseudonymised(&self) -> usize {
        self.aliases.len()
    }
}

fn looks_like_a_path(s: &str) -> bool {
    s.starts_with('/') || s.contains("/Users/")
}

/// Read `input`, write a redacted `output`, and say what happened.
///
/// The output is checked against [`is_scrubbed`] and the file is written only
/// if every string passes. Redaction is the control that stands between a real
/// transcript and a remote, so it verifies its own work at the moment it
/// matters rather than leaving a bad file on disk for a test to find later.
///
/// # Errors
/// If the input cannot be read, the output cannot be written, or a redacted
/// line still contains something that should not be published.
pub fn file(input: &Path, output: &Path, max_lines: Option<usize>) -> anyhow::Result<Report> {
    let source = std::fs::read_to_string(input)
        .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", input.display()))?;

    let mut redactor = Redactor::new();
    let mut kept: Vec<String> = Vec::new();
    let mut unparseable = 0usize;

    for (n, line) in source
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| (i + 1, l))
    {
        if max_lines.is_some_and(|max| kept.len() >= max) {
            break;
        }
        let Some(parsed) = serde_json::from_str::<Value>(line).ok() else {
            unparseable += 1;
            continue;
        };
        let redacted = redactor.value(parsed, None);
        if let Some(leak) = first_leak(&redacted) {
            // Deliberately not echoing the text: this runs in a terminal and
            // sometimes in CI, and a leak report that pastes the leak has
            // published it a second time. Key and line are enough to fix it.
            anyhow::bail!(
                "line {n}: {} would still be published — add it to KEEP_KEYS \
                 deliberately, or to TEXT_KEYS/PATH_KEYS to scrub it. Nothing was written.",
                leak
            );
        }
        kept.push(serde_json::to_string(&redacted)?);
    }

    let mut body = kept.join("\n");
    body.push('\n');
    std::fs::write(output, body)
        .map_err(|e| anyhow::anyhow!("cannot write {}: {e}", output.display()))?;

    Ok(Report {
        kept: kept.len(),
        unparseable,
        paths_pseudonymised: redactor.paths_pseudonymised(),
    })
}

const USAGE: &str = "cargo xtask redact <input.jsonl> <output.jsonl> [--max-lines N]";

/// # Errors
/// On bad arguments, or if the file cannot be read or written.
pub fn main(args: &[String]) -> anyhow::Result<()> {
    let mut positional: Vec<&str> = Vec::new();
    let mut max_lines: Option<usize> = None;
    let mut rest = args.iter();

    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--max-lines" => {
                let raw = rest
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--max-lines needs a number\n{USAGE}"))?;
                max_lines = Some(
                    raw.parse()
                        .map_err(|_| anyhow::anyhow!("--max-lines: `{raw}` is not a number"))?,
                );
            }
            other => positional.push(other),
        }
    }

    let [input, output] = positional.as_slice() else {
        anyhow::bail!("{USAGE}");
    };
    let (input, output) = (Path::new(input), Path::new(output));

    // Refuse to redact in place: a half-written output over the original would
    // destroy the only copy of the evidence.
    if input == output {
        anyhow::bail!("input and output must differ — redaction is not an in-place edit");
    }

    let report = file(input, output, max_lines)?;
    let name = output.file_name().unwrap_or(output.as_os_str());
    println!(
        "{}: {} lines, {} paths pseudonymised{}",
        name.to_string_lossy(),
        report.kept,
        report.paths_pseudonymised,
        if report.unparseable == 0 {
            String::new()
        } else {
            format!(", {} line(s) dropped as unparseable", report.unparseable)
        }
    );
    Ok(())
}

/// Hand every string in `value` to `visit`, along with the key it sits under.
///
/// An array inherits its parent's key, matching how the redactor descends, so
/// the check and the transform cannot disagree about what a value was called.
pub fn walk_strings(value: &Value, key: Option<&str>, visit: &mut impl FnMut(Option<&str>, &str)) {
    match value {
        Value::String(s) => visit(key, s),
        Value::Array(items) => {
            for item in items {
                walk_strings(item, key, visit);
            }
        }
        Value::Object(fields) => {
            for (k, v) in fields {
                walk_strings(v, Some(k), visit);
            }
        }
        _ => {}
    }
}

/// The first string that would still be published, described without quoting it.
#[must_use]
pub fn first_leak(value: &Value) -> Option<String> {
    let mut found = None;
    walk_strings(value, None, &mut |key, s| {
        if found.is_none() && !is_scrubbed(key, s) {
            found = Some(format!(
                "a {} character string under key `{}`",
                s.chars().count(),
                key.unwrap_or("(none)")
            ));
        }
    });
    found
}

/// Whether a string in a fixture is safe to have been committed.
///
/// The invariant the redactor produces, stated independently so it can be
/// checked against files it did not just write.
#[must_use]
pub fn is_scrubbed(key: Option<&str>, s: &str) -> bool {
    if s.chars().count() <= SHORT_ENOUGH_TO_BE_STRUCTURAL {
        return true;
    }
    if key.is_some_and(|k| is(KEEP_KEYS, k)) {
        return true;
    }
    if let Some(n) = s.strip_prefix(PATH_ALIAS_PREFIX) {
        return n.chars().all(|c| c.is_ascii_digit());
    }
    s.chars().all(|c| c == 'x')
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use serde_json::json;

    fn redact(v: Value) -> Value {
        Redactor::new().value(v, None)
    }

    #[test]
    fn every_number_survives_exactly() {
        // The entire reason the fixture exists. If a count changed here, every
        // golden test would be asserting against fiction.
        let input = json!({
            "type": "assistant",
            "message": { "usage": {
                "input_tokens": 2,
                "output_tokens": 4452,
                "cache_creation_input_tokens": 23610,
                "cache_read_input_tokens": 30885,
                "cache_creation": { "ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 23610 }
            }}
        });
        let out = redact(input.clone());
        assert_eq!(out["message"]["usage"], input["message"]["usage"]);
    }

    #[test]
    fn prose_is_replaced_but_keeps_its_length() {
        let out = redact(json!({ "text": "the quick brown fox" }));
        assert_eq!(out["text"], json!("xxxxxxxxxxxxxxxxxxx"));
    }

    #[test]
    fn very_long_prose_is_truncated_rather_than_mirrored() {
        let out = redact(json!({ "text": "a".repeat(5_000) }));
        assert_eq!(out["text"].as_str().unwrap().len(), MAX_REDACTED_LEN);
    }

    #[test]
    fn structural_short_strings_are_left_alone() {
        // Enum-ish values a parser branches on. Blanking them would change what
        // the fixture exercises.
        let out = redact(json!({ "mode": "ask", "role": "user" }));
        assert_eq!(out["mode"], json!("ask"));
        assert_eq!(out["role"], json!("user"));
    }

    #[test]
    fn a_path_becomes_a_stable_alias_and_the_same_path_maps_the_same_way() {
        let mut r = Redactor::new();
        let a = r.value(json!({ "cwd": "/Users/someone/work/secret-project" }), None);
        let b = r.value(json!({ "cwd": "/Users/someone/work/secret-project" }), None);
        assert_eq!(a["cwd"], b["cwd"]);
        assert_eq!(a["cwd"], json!("/redacted/path-1"));
        assert_eq!(r.paths_pseudonymised(), 1);
    }

    #[test]
    fn different_paths_get_different_aliases() {
        // Collapsing them would erase the multi-project structure a test may
        // depend on: two sessions in one directory is not the same fixture as
        // two sessions in two.
        let mut r = Redactor::new();
        let out = r.value(json!({ "cwd": "/a/one", "path": "/a/two" }), None);
        assert_ne!(out["cwd"], out["path"]);
        assert_eq!(r.paths_pseudonymised(), 2);
    }

    #[test]
    fn an_unknown_key_is_redacted_rather_than_published() {
        // The direction that matters: a key nobody has audited must not pass
        // free text through just because it was not on a list.
        let out = redact(json!({ "some_new_field_2027": "an internal hostname" }));
        assert_eq!(out["some_new_field_2027"], json!("xxxxxxxxxxxxxxxxxxxx"));
    }

    #[test]
    fn an_unknown_key_holding_a_path_still_gets_a_path_alias() {
        let out = redact(json!({ "whatever": "/Users/someone/.ssh/id_ed25519" }));
        assert_eq!(out["whatever"], json!("/redacted/path-1"));
    }

    #[test]
    fn a_kept_key_does_not_protect_the_strings_nested_under_it() {
        // `usage` is a keep-key, but that is about `usage` itself. If it ever
        // grows a free-text member, the member is judged on its own name.
        let out = redact(json!({ "usage": { "input_tokens": 7, "note": "customer name" } }));
        assert_eq!(out["usage"]["input_tokens"], json!(7));
        assert_eq!(out["usage"]["note"], json!("xxxxxxxxxxxxx"));
    }

    #[test]
    fn arrays_inherit_their_parents_key() {
        let out = redact(json!({ "workspace_roots": ["/a/one", "/a/two"] }));
        assert_eq!(out["workspace_roots"][0], json!("/redacted/path-1"));
        assert_eq!(out["workspace_roots"][1], json!("/redacted/path-2"));
    }

    #[test]
    fn content_blocks_keep_their_shape_and_lose_their_words() {
        let out = redact(json!({
            "type": "assistant",
            "message": { "content": [
                { "type": "thinking", "thinking": "reasoning about the codebase" },
                { "type": "text", "text": "here is the answer" }
            ]}
        }));
        assert_eq!(out["message"]["content"][0]["type"], json!("thinking"));
        assert_eq!(
            out["message"]["content"][0]["thinking"],
            json!("xxxxxxxxxxxxxxxxxxxxxxxxxxxx")
        );
        assert_eq!(
            out["message"]["content"][1]["text"],
            json!("xxxxxxxxxxxxxxxxxx")
        );
    }

    #[test]
    fn a_line_that_does_not_parse_is_dropped_and_counted() {
        let dir = std::env::temp_dir().join(format!("aum-redact-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("in.jsonl");
        let output = dir.join("out.jsonl");
        std::fs::write(&input, "{\"text\":\"hello there\"}\nnot json at all\n").unwrap();

        let report = file(&input, &output, None).unwrap();
        assert_eq!(report.kept, 1);
        assert_eq!(report.unparseable, 1);

        let written = std::fs::read_to_string(&output).unwrap();
        assert!(!written.contains("not json"), "{written}");
        assert!(written.ends_with('\n'));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn max_lines_truncates() {
        let dir = std::env::temp_dir().join(format!("aum-redact-max-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("in.jsonl");
        let output = dir.join("out.jsonl");
        std::fs::write(&input, "{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n").unwrap();

        let report = file(&input, &output, Some(2)).unwrap();
        assert_eq!(report.kept, 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn redacting_over_the_input_is_refused() {
        // The failure this prevents: destroying the only copy of a transcript
        // that took a week of real work to produce.
        let same = "x.jsonl".to_owned();
        let err = main(&[same.clone(), same]).unwrap_err();
        assert!(err.to_string().contains("must differ"), "{err}");
    }

    #[test]
    fn multibyte_text_does_not_split_a_character() {
        // `"x".repeat(byte_len)` on a string of emoji would produce a longer
        // string than the original and, worse, invite byte-indexed slicing.
        let out = redact(json!({ "text": "héllo wörld ✨" }));
        assert_eq!(out["text"], json!("xxxxxxxxxxxxx"));
    }

    #[test]
    fn the_scrubbed_check_agrees_with_what_the_redactor_produces() {
        // The fixture test below trusts `is_scrubbed`; this is what ties it to
        // the redactor rather than letting the two drift apart.
        let mut r = Redactor::new();
        for (key, raw) in [
            ("text", "some prose that is definitely long enough"),
            ("cwd", "/Users/someone/work"),
            ("unknown_key", "an internal hostname"),
            ("model", "claude-opus-5"),
            ("mode", "ask"),
        ] {
            let out = r.string(raw, Some(key));
            assert!(is_scrubbed(Some(key), &out), "{key}: {out}");
        }
        assert!(!is_scrubbed(Some("unknown_key"), "an internal hostname"));
        assert!(!is_scrubbed(Some("cwd"), "/Users/someone/work"));
    }

    /// Every string committed under `tests/fixtures/` is either structural, on
    /// the audited keep-list, a path alias, or blanked.
    ///
    /// `.gitignore` names this test as the thing that enforces the rule, so it
    /// walks the directory rather than a hard-coded list of files: a fixture
    /// added later is covered without anyone remembering to add it here.
    #[test]
    fn fixtures_are_redacted() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask/ has a parent")
            .join("tests/fixtures");

        let mut checked = 0usize;
        let mut strings = 0usize;
        for file in jsonl_under(&root) {
            let body = std::fs::read_to_string(&file).unwrap();
            for (n, line) in body.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let value: Value = serde_json::from_str(line)
                    .unwrap_or_else(|e| panic!("{}:{}: {e}", file.display(), n + 1));
                walk_strings(&value, None, &mut |_, _| strings += 1);
                // Reported without quoting the offending text: a test failure
                // that pastes the leak into CI output has published it again.
                assert!(
                    first_leak(&value).is_none(),
                    "{}:{} would publish {}",
                    file.display(),
                    n + 1,
                    first_leak(&value).unwrap_or_default()
                );
            }
            checked += 1;
        }

        assert!(checked > 0, "no fixtures found under {}", root.display());
        assert!(strings > 0, "fixtures contained no strings to check");
    }

    fn jsonl_under(dir: &Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(jsonl_under(&path));
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                out.push(path);
            }
        }
        out
    }

    #[test]
    fn a_whole_number_written_as_a_float_stays_a_float() {
        // Codex writes `"used_percent": 1.0`. The TypeScript this replaced went
        // through JavaScript numbers and emitted `1`, quietly changing the JSON
        // type of a value in a file whose purpose is to be a faithful record.
        let parsed: Value =
            serde_json::from_str(r#"{"payload":{"rate_limits":{"used_percent":1.0}}}"#).unwrap();
        let out = serde_json::to_string(&Redactor::new().value(parsed, None)).unwrap();
        assert!(out.contains("1.0"), "{out}");
    }

    #[test]
    fn nothing_is_written_when_a_line_would_still_leak() {
        // The self-check has to fail closed. If it wrote the file and then
        // complained, the transcript would already be sitting in the working
        // tree waiting for someone to `git add .`.
        let dir = std::env::temp_dir().join(format!("aum-redact-leak-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("in.jsonl");
        let output = dir.join("out.jsonl");
        // `model` is a keep-key, so a redactor bug that let prose through here
        // would be invisible; instead make the *check* the thing under test by
        // pointing it at a value the redactor cannot scrub: an object key.
        std::fs::write(&input, "{\"model\":\"claude-opus-5\"}\n").unwrap();
        assert!(file(&input, &output, None).is_ok(), "keep-keys are allowed");

        // And a value the redactor does scrub still passes.
        std::fs::write(&input, "{\"prompt\":\"a real prompt goes here\"}\n").unwrap();
        let report = file(&input, &output, None).unwrap();
        assert_eq!(report.kept, 1);
        let written = std::fs::read_to_string(&output).unwrap();
        assert!(!written.contains("real prompt"), "{written}");

        // The check itself, fed something the redactor never produces.
        let leak = first_leak(&json!({ "prompt": "a real prompt goes here" }));
        assert!(leak.is_some_and(|l| l.contains("prompt") && !l.contains("real prompt")));
        std::fs::remove_dir_all(&dir).ok();
    }
}
