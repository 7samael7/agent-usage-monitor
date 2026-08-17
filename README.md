# agent-usage-monitor

A private, local-first terminal application that measures, records and prices the AI token usage
produced by coding agents running on your own machine — Claude Code and Codex today, others as
adapters are added.

It reads what those agents already write to disk. It launches nothing, sends nothing, and needs no
account.

> **The rule this project is built around:** a number is never presented as more certain than it is.
> Every measurement carries its source. Missing data is shown as *unavailable* — never as zero, and
> never quietly replaced by an estimate.

```
$ aum overview

Usage — all time

  requests       44,639   (55 failed and could not be measured)
  total tokens   8,853,828,550
  input side     8,817,956,640
  output         32,789,248
  reasoning      ≥3,647,647
  API-equivalent ≥$6531.14
  actually billed —  subscription, not billed per token

Top models
model                      requests         tokens   reasoning       cost  rate /Mtok (USD)
claude-opus-5                15,031  3,814,180,913           —  ≈$2746.17  $5 in / $25 out
gpt-5.6-terra                13,830  2,016,295,303  ≥2,264,436   ≥$572.83  $2 in / $12 out
claude-fable-5                3,047    972,048,584           —  ≈$1420.20  $10 in / $50 out

exact · ≈ calculated · ≥ at least · — not measured
```

Every figure says how well it is known. `≥3,647,647` is a floor, because Codex reports reasoning
tokens and Claude Code reports none at all; summing them would produce something that looks like a
measurement and isn't. `≈` marks a cost calculated from a rate you can see and change. `—` is not
zero. And the 55 requests that failed are named as unmeasured rather than folded in as free.

`aum` with no subcommand opens the same data as a full-screen interface: seven tabs, bar charts per
day and hour, and a year-wide contribution graph.

## Installing

Requirements: Rust 1.92, pinned in `rust-toolchain.toml`. `make doctor` checks.

```bash
make install
```

That is `cargo install --path crates/aum-tui`, which puts `aum` in `~/.cargo/bin`. Then, in any
terminal:

```bash
aum
```

`Tab`/`←→` or `1`–`7` to move between tabs, `↑↓` for rows, `h` to swap daily and hourly, `r` to
refresh, `e` to export the current view as JSON, `?` for the full key list, `q` to quit. Numbers
update on their own while agents work.

The daily table opens with **today at the top**, and `d` `n` `t` `c` re-sort it by date, requests,
tokens or cost — the same key again reverses it, and the sorted column is marked in its header. The
chart above stays in time order whatever the table does. A row whose cost could not be worked out
sorts to the *end* either way: an unpriced row is not a cheap one.

## From a script

Every tab is also a subcommand, and every subcommand takes `--json`:

```bash
aum daily --week                    # a table
aum models --json                   # the same data, machine-readable
aum sessions --since 2026-08-01 --adapter codex
aum overview --currency EUR
aum sync                            # one ingest pass, then exit — for cron
```

Ranges: `--today`, `--week`, `--month`, `--year 2026`, or `--since`/`--until` with a date or an
RFC 3339 timestamp. `--no-color` and `NO_COLOR` are both honoured; the certainty markers are text, so
nothing is lost without colour.

`--sort date|requests|tokens|cost` and `--reverse` order the rows, and the output says which order it
used. Printed tables are chronological by default — the opposite of the interactive view, because
printed rows scroll and the *last* one is the one left beside your prompt:

```bash
aum daily --reverse                 # today first
aum models --sort cost              # where the money went
```

Prices ship for the models these agents use, and can be overridden:

```bash
aum price gpt-5.6-terra --input 2.00 --output 12.00 --cache-read 0.20
aum fx EUR 0.92
```

## What it measures, and how honestly

| Application | Detected | Exact tokens | Model | API-equivalent cost | Latency |
|---|---|---|---|---|---|
| Claude Code | yes | **yes** — provider usage relayed to disk | yes | yes | no |
| Codex (CLI, desktop, VS Code) | yes | **yes** — including reasoning tokens | yes | yes | no |
| Claude Desktop | yes | **no** — a daily total only, not per request | no | no | no |

This table is not marketing copy. The **Apps** tab generates the same matrix at runtime from what
each adapter actually observes in real data, with the evidence beside each row, and it will disagree
with this README if the tools change underneath it.

**Claude Desktop deserves the emphasis.** It writes plan-limit percentages, from which there is no
defensible conversion to a token count, and one running token total for the current day. That total
is real and the app shows it — as itself, on the Apps tab, with what it covers stated beside it. It
carries no model, no input/output split and no conversation, so it can never be priced, and it is
kept out of every aggregate that could imply otherwise. The app keeps the history, because Claude
Desktop discards the counter at midnight.

**Latency is unavailable everywhere**, and stays that way. Neither agent records it. The gap between
two message timestamps contains tool execution, retry backoff and user think time, so dividing output
tokens by it yields a plausible, well-scaled, entirely wrong tokens-per-second figure. That would
need OpenTelemetry ingestion or a local proxy, neither of which is built.

There is also **no tokenizer counting**, and that is a decision rather than a gap. A tokenizer only
produces an estimate, and an estimate is worth having exactly where nothing better exists — which is
nowhere here. Both agents report provider-authored counts, where a disagreeing estimate would be
strictly worse; the one source without per-request counts, Claude Desktop, exposes no text to count
either.

## Why there is no traffic sniffing

Watching encrypted traffic from a closed desktop client would require terminating its TLS connection —
installing a root certificate and impersonating the provider. This project does not do that, and it
would not even work: token accounting happens server-side against the fully-assembled prompt,
including system prompts and tool schemas the client never sees. Counting the visible request body
locally gives a number that is structurally too low, and calling it exact would be a lie with a
plausible shape.

See [`docs/capture-methods.md`](docs/capture-methods.md).

## Privacy

No account. No telemetry. No analytics. No cloud database. No remote backend. No network listener —
there is no server and no port.

One local SQLite file, two directories read. Prompt text, response text and tool input/output are
**not** recorded: metadata only. See [`docs/privacy.md`](docs/privacy.md).

## Architecture in one paragraph

One binary. `aum` opens the SQLite database, starts the ingest engine on a background task, and draws
from it; the CLI subcommands run the same queries and print instead. There is no server, no IPC and
no serialization boundary inside the process — the interface calls the engine directly.

```
crates/
  aum-contract    Measured, Accuracy, Money, TokenBands — the vocabulary, zero dependencies
  aum-domain      TokenUsage normalization  (pure, no I/O)
  aum-db          SQLite, migrations, single write actor, aggregation queries
  aum-ingest      file tailer: cursors, partial lines, prefilter, batching
  aum-adapters    claude_code · codex · claude_desktop
  aum-pricing     versioned prices, FX, decimal cost engine
  aum-engine      adapter lifecycle, capability probing, costing views
  aum-tui         the `aum` binary: CLI and Ratatui interface
xtask/            repository chores — `cargo xtask redact`
```

## Development

```bash
make check                 # fmt --check, clippy -D warnings, and the whole test suite
cargo test
cargo run -p aum-tui       # run without installing
```

Tests that read this machine's own agent data are ignored by default, because they need data that
only exists where the agents have really run:

```bash
cargo test -p aum-adapters --test real_corpus -- --ignored --nocapture
cargo test -p aum-engine   --test probe_real  -- --ignored --nocapture
```

## Contributing a fixture

Golden-file tests are built from real agent transcripts, which contain prompts, responses and source
code. **Run every transcript through `cargo xtask redact` before it goes anywhere near this
repository:**

```bash
make redact IN=~/.claude/projects/<slug>/<session>.jsonl OUT=tests/fixtures/claude_code/<name>.jsonl
```

Redaction keeps every number and structural field and replaces all free text. It verifies its own
output and writes nothing if anything would still be published; `cargo test fixtures_are_redacted`
re-checks what is already committed, and `.gitignore` blocks `*.jsonl` outside `tests/fixtures/`.

## Documentation

| Document | Contents |
|---|---|
| [`docs/architecture.md`](docs/architecture.md) | design, data sources, normalization, attribution |
| [`docs/capture-methods.md`](docs/capture-methods.md) | the four capture levels and per-adapter fidelity |
| [`docs/privacy.md`](docs/privacy.md) | what is stored, where, and what leaves the machine |
| [`docs/comparing-agents.md`](docs/comparing-agents.md) | which columns may honestly be compared between agents |
| [`docs/adapter-development.md`](docs/adapter-development.md) | adding support for a new AI application |
| [`docs/pricing.md`](docs/pricing.md) | pricing model, versioning, currencies |

## Licence

MIT. Private project; not accepting external contributions at this stage.
