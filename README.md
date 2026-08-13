# agent-usage-monitor

A private, local-first desktop application that measures, records, visualises and compares AI token
usage produced by AI agents running on your own machine — Claude Code, Codex, and other local AI
coding tools.

It is part usage monitor, part session profiler, part cost analyser, part benchmark harness. You can
run two agents at the same task, side by side, and see exactly what each one consumed.

> **The rule this project is built around:** a number is never presented as more certain than it is.
> Every measurement carries its source. Missing data is shown as *unavailable* — never as zero, and
> never quietly replaced by an estimate.

## Status

Working. It reads both agents' usage, launches and monitors tasks, prices what it can, and shows
what it found — including what it could not measure. Verified against a real corpus of 43,761
requests across 522 files.

Not yet built: the local proxy for third-party clients, and OpenTelemetry ingestion (which is what
would make latency measurable). See [`docs/architecture.md`](docs/architecture.md) for the design.

There is also **no tokenizer counting**, and that is a decision rather than a gap. A tokenizer can
only produce an estimate, and an estimate is worth having exactly where nothing better exists —
which is nowhere here. Both agents report provider-authored counts, where an estimate that disagreed
would be strictly worse; the one source that reports nothing, Claude Desktop, also exposes no text to
count. `TokenizerCalculated` remains in the wire contract so a different backend can report it and
this interface will render it as *Calculated*, but this backend never produces it.

## What it measures, and how honestly

| Application | Detected | Exact tokens | Model | API-equivalent cost | Latency |
|---|---|---|---|---|---|
| Claude Code | yes | **yes** — provider usage relayed to disk | yes | yes | needs OTEL or proxy |
| Codex (CLI, desktop, VS Code) | yes | **yes** — including reasoning tokens | yes | yes | needs OTEL or proxy |
| Claude Desktop | yes | **no** — exposes no token telemetry | no | no | no |
| Generic OpenAI/Anthropic client | via proxy | yes, when routed through the local proxy | yes | yes | yes |

This table is not marketing copy. The application generates its own capability matrix at runtime from
what each adapter actually observes in real data, and the Applications screen will disagree with this
README if the tools change underneath it.

**Claude Desktop deserves the emphasis.** It writes only plan-limit percentages — there is no
defensible conversion from "54% of a five-hour window" to a token count, so the app reports
*unavailable* rather than inventing one.

## Why there is no traffic sniffing

Watching encrypted traffic from a closed desktop client would require terminating its TLS connection —
installing a root certificate and impersonating the provider. This project does not do that, and it
would not even work: token accounting happens server-side against the fully-assembled prompt, including
system prompts and tool schemas the client never sees. Counting the visible request body locally gives a
number that is structurally too low, and calling it exact would be a lie with a plausible shape.

Instead the app reads what applications already choose to write down, and offers an opt-in local proxy
for clients you configure yourself. See [`docs/capture-methods.md`](docs/capture-methods.md).

## Privacy

No account. No telemetry. No analytics. No cloud database. No remote backend.

Everything is stored in a local SQLite file. Prompt text, response text and tool input/output are
**not** recorded by default — only metadata. The renderer process is structurally prevented from
reaching the network: a CSP plus a request filter in the Electron main process cancels anything that is
not the local sidecar, and the count of blocked attempts is visible in Settings. The only outbound
network the application makes at all is exchange-rate and pricing updates, each individually
disableable.

See [`docs/privacy.md`](docs/privacy.md).

## Architecture in one paragraph

An Electron app handles lifecycle, windows and native integration, and supervises a **Rust sidecar**
that holds all the business logic. They speak over loopback HTTP + SSE, bootstrapped by a single line of
JSON on the sidecar's stdout. The contract between them is an OpenAPI document, which is what makes the
backend replaceable — a Go, C# or Python implementation serving the same document needs no changes
anywhere in the desktop app.

```
crates/
  aum-contract    wire types only, zero internal dependencies — the replaceable boundary
  aum-domain      TokenUsage, Money, MeasurementSource, normalization  (pure, no I/O)
  aum-db          SQLite, migrations, single write actor
  aum-ingest      file tailer: cursors, partial lines, prefilter, batching
  aum-adapters    claude_code · codex · claude_desktop
  aum-procmon     process trees, launching, (pid, start_time) identity
  aum-pricing     versioned prices, FX, decimal cost engine
  aum-engine      task supervisor, bindings, adapter lifecycle, event bus
  aum-server      axum router, SSE, auth, handshake
  aum-sidecar     the binary
apps/desktop      Electron + React + TypeScript + Vite
packages/api-contract   generated OpenAPI + TypeScript
```

## Development

Requirements: Rust 1.92 (pinned in `rust-toolchain.toml`), bun 1.3+, Node 22+.

```bash
cargo test                 # backend
cargo clippy --all-targets # lints
bun install                # desktop dependencies
bun run dev                # Electron in development
```

Tests that read this machine's own agent data are ignored by default, because
they need data that only exists where the agents have really run:

```bash
cargo test -p aum-adapters --test real_corpus -- --ignored --nocapture
cargo test -p aum-engine   --test probe_real  -- --ignored --nocapture
```

To package for macOS ARM64:

```bash
bun run --cwd apps/desktop package
```

The sidecar is spawned as a compiled binary, never via `cargo run` — `cargo run` writes build output to
stdout, which would corrupt the handshake line. Rebuild it in a separate terminal:

```bash
cargo build -p aum-sidecar
```

## Contributing a fixture

Golden-file tests are built from real agent transcripts, which contain prompts, responses and source
code. **Run every fixture through `scripts/redact-fixture.ts` before it goes anywhere near the
repository.** Redaction preserves every number and structural field and replaces all free text; a test
asserts that nothing unredacted is present, and `.gitignore` blocks `*.jsonl` outside
`tests/fixtures/`.

## Documentation

| Document | Contents |
|---|---|
| [`docs/architecture.md`](docs/architecture.md) | design, data sources, normalization, attribution |
| [`docs/capture-methods.md`](docs/capture-methods.md) | the four capture levels and per-adapter fidelity |
| [`docs/privacy.md`](docs/privacy.md) | what is stored, where, and what leaves the machine |
| [`docs/benchmarking.md`](docs/benchmarking.md) | running comparisons and their comparability caveats |
| [`docs/adapter-development.md`](docs/adapter-development.md) | adding support for a new AI application |
| [`docs/pricing.md`](docs/pricing.md) | pricing model, versioning, currencies |

## Licence

MIT. Private project; not accepting external contributions at this stage.
