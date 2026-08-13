# Architecture

`agent-usage-monitor` measures, records and compares AI token usage produced by AI agents running
locally on this machine. It is private and local-first: no account, no telemetry, no cloud database, no
remote backend.

Its defining constraint is not a feature. It is a refusal:

> **A number is never presented as more certain than it is.** Every measurement carries a
> `MeasurementSource`. Missing data is rendered as *unavailable*, never as zero, and never quietly
> replaced by an estimate.

That constraint is what most of this document is about, because the naive implementation of nearly every
part of this system produces confident, well-formatted, wrong numbers.

---

## 1. What data actually exists

Before designing anything we measured what these tools expose on a real machine (macOS 26.5, M1 Pro).
The findings below are empirical, taken from a live corpus of 454 Claude Code transcripts (325 MiB) and
59 Codex rollouts (986 MiB). They are the foundation of every design decision that follows.

### 1.1 Claude Code — exact, real-time, on disk

Claude Code writes a JSONL transcript per session:

```
~/.claude/projects/<cwd-with-slashes-replaced-by-dashes>/<session-uuid>.jsonl
```

Each `type: "assistant"` line carries `.message.usage` — **the Anthropic API's own usage object, relayed
verbatim**:

```jsonc
{
  "input_tokens": 2,
  "output_tokens": 4452,
  "cache_creation_input_tokens": 23610,
  "cache_read_input_tokens": 30885,
  "cache_creation": { "ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 23610 },
  "server_tool_use": { "web_search_requests": 0, "web_fetch_requests": 0 },
  "service_tier": "standard",
  "iterations": [ /* breakdown of THIS message — never an addend */ ]
}
```

The file is appended in real time, one line per message, with no persistent open file descriptor
(open → append → close). Tailing it yields exact usage within roughly one assistant message of latency.

**Not present:** reasoning/thinking token counts, request latency, and time-to-first-token. Claude Code
emits `thinking` content blocks but no count for them. This is a permanent *Unavailable*, not a gap we
can fill.

**Subagent usage lives in separate files:**

```
~/.claude/projects/<slug>/<session-uuid>/subagents/agent-<id>.jsonl
~/.claude/projects/<slug>/<session-uuid>/subagents/workflows/wf_<id>/agent-<id>.jsonl
```

In the measured corpus these are 323 of 454 files and carry 15–37 % of all tokens depending on the
metric. Crucially, **their lines carry the *parent* session's `sessionId`**, plus `isSidechain: true`
and `agentId`. Attribution keyed on the per-line `sessionId` therefore picks them up automatically;
attribution keyed on file path does not.

Live sessions are discoverable at `~/.claude/sessions/<PID>.json`, which maps a running process to its
session UUID — this is the primitive that makes *attach to a running agent* possible.

Claude Code also supports OpenTelemetry (`CLAUDE_CODE_ENABLE_TELEMETRY=1`), emitting
`claude_code.token.usage` with `type ∈ input | output | cacheRead | cacheCreation` and
`claude_code.cost.usage` in USD. Content logging is off by default, so the OTEL path carries counts and
metadata only. This is a post-MVP capture level; it is the only route to per-request latency for Claude
Code.

### 1.2 Codex — exact, real-time, on disk, but cumulative

Codex writes rollout files:

```
~/.codex/sessions/YYYY/MM/DD/rollout-<timestamp>-<uuid>.jsonl
```

After every model turn it emits an `event_msg` with `payload.type == "token_count"`:

```jsonc
{
  "info": {
    "total_token_usage": { /* CUMULATIVE for the session */
      "input_tokens": 16210, "cached_input_tokens": 11008, "cache_write_input_tokens": 0,
      "output_tokens": 164, "reasoning_output_tokens": 35, "total_tokens": 16374 },
    "last_token_usage": { /* delta for the most recent API call */ },
    "model_context_window": 258400
  },
  "rate_limits": { "primary": { "used_percent": 41.2, "window_minutes": 300 }, "plan_type": "plus" }
}
```

Codex **does** report reasoning tokens; Claude Code does not. That asymmetry is real and must survive
into the UI rather than being flattened to zero.

Note the model is **not** on the usage event — it lives on `turn_context` lines and must be carried
forward. A tailer that starts mid-file has no model until the next `turn_context`, and therefore cannot
compute cost. That is an honest *Unavailable*, not a reason to guess.

The Codex binary is **not on `PATH`** on this machine. It ships inside the ChatGPT desktop app at
`/Applications/ChatGPT.app/Contents/Resources/codex` (bundle id `com.openai.codex` — ChatGPT.app *is*
the Codex desktop app). Usage driven from the GUI and from the VS Code extension lands in the same
rollout files.

### 1.3 Claude Desktop — no token data at all

Claude Desktop stores exactly one quantitative usage artifact:

```
~/Library/Application Support/Claude/plan-usage-history.json
  { "version": 2, "samples": [ { "t": 1786561845994, "org": "<uuid>", "u": { "fh": 54, "sd": 67 } } ] }
```

`fh` and `sd` are percentages of the five-hour and seven-day plan windows. There is **no defensible
transformation from "54 % of a five-hour window" to a token count**. Claude Desktop's row in the
capability matrix reads *Token telemetry: Unavailable*, with that sentence as the reason. Its
plan-percentage data may be shown, clearly labelled, in a widget that is not a token widget.

---

## 2. Why encrypted desktop traffic cannot give exact tokens

A reasonable question is why the monitor does not simply watch the network. It is worth answering
precisely, because the answer determines the honest ceiling of what this app can claim.

**Observing that traffic exists is not observing what it contains.** A closed desktop client talks to its
provider over TLS. To read the request and response bodies, the monitor would have to terminate that TLS
connection itself — install a root certificate, re-sign the traffic, and impersonate the provider. That
is a man-in-the-middle attack on another application. This project does not do it, and will not:

- It requires modifying another application's trust store or certificate validation.
- It exposes the user's authentication tokens and full conversation content to a third process.
- It breaks certificate pinning, which several of these clients use, in ways that are indistinguishable
  from an attack.
- It would decrypt unrelated HTTPS traffic as collateral.

**And even if it were done, it would not yield exact counts for the general case.** Token accounting is
performed server-side by the provider's tokenizer against the fully-assembled prompt — including the
system prompt, tool schemas, and any server-side injections that the client never sees. Counting the
visible request body locally reproduces neither. It gives a *calculated* number, structurally lower than
the truth, and calling it exact would be a lie with a plausible shape.

So the ceiling is set by what an application chooses to tell us:

- An application that **writes its provider-reported usage to disk** (Claude Code, Codex) can be measured
  **exactly**, with zero interception, because the provider's own numbers are sitting in a file.
- An application that **exposes no usage** (Claude Desktop) is **Unavailable**, and process detection
  alone does not change that. Detecting a running process proves a process is running. It proves nothing
  about tokens.
- An application the user **explicitly routes through our proxy** can be measured exactly from the
  protocol's own `usage` field — because the user configured that, knowingly, for their own client. This
  is consent-based observation, not interception.

The honest capability matrix in the Applications screen is generated from what adapters actually observe
at runtime, not from a hardcoded table of optimistic claims.

---

## 3. Measurement sources

Classification is by **authorship of the number** — who computed it — not by the medium it travelled
through.

| Variant | Definition | Displayed as |
|---|---|---|
| `ProviderReported` | Produced by the provider's own serving/billing infrastructure and relayed to us without recomputation. | **Exact** |
| `ProtocolMetadata` | Read by us off the client↔provider wire (our proxy reading the protocol's `usage` field). Provider-authored, observed at the transport. | **Exact** |
| `ApplicationTelemetry` | Computed by the *agent application*, not the provider — e.g. a cost figure the CLI derived from its own price table. | **Calculated** |
| `TokenizerCalculated` | We ran the model's real tokenizer over text we legitimately possess. Deterministic, but blind to system prompts and tool schemas. | **Calculated** |
| `Estimated` | Heuristic approximation. Only ever produced when the user explicitly opts in. | **Estimated** |
| `Unavailable` | We know a request happened and have no number for it. **Distinct from zero.** | **Unavailable** |

The interesting case is Claude Code's transcript, and it demonstrates that the rule does real work:

- `.message.usage` is **`ProviderReported`**. It contains `service_tier` and `inference_geo` — fields the
  CLI cannot compute and can only have received. When one response is written to eight lines, all eight
  carry byte-identical usage: the CLI is copying a value, not deriving one.
- `claude_code.cost.usage`, from the same emitter, is **`ApplicationTelemetry`**. Anthropic never sent a
  dollar figure for subscription usage; the CLI computed it from its own table.

Same file, same application, different authorship, different classification.

### 3.1 Every data path, classified

| Path | Source | Display |
|---|---|---|
| Claude Code transcript `.message.usage` | `ProviderReported` | Exact |
| Claude Code subagent `agent-*.jsonl` usage | `ProviderReported` | Exact |
| Claude Code `<synthetic>` line (API failure marker) | `Unavailable` | Unavailable |
| Claude Code `api_error` line (failed attempt) | `Unavailable` | Unavailable |
| Claude Code `compact_boundary.preTokens` | *not a usage measurement* — context annotation | n/a |
| OTEL `claude_code.token.usage` | `ProviderReported` (aggregated relay) | Exact, completeness-flagged |
| OTEL `claude_code.cost.usage` | `ApplicationTelemetry` | Calculated |
| Codex `token_count` cumulative delta | `ProviderReported` | Exact |
| Codex off-ledger compaction call | `ProviderReported` | Exact |
| Codex `rate_limits.*` | `ApplicationTelemetry` (not tokens) | Calculated |
| Claude Desktop `plan-usage-history.json` | *not a token measurement* | Unavailable |
| Proxy-observed protocol `usage` | `ProtocolMetadata` | Exact |

### 3.2 Aggregate accuracy

Per-request classification is not sufficient. Two axes must both be clean before a *total* may call
itself exact:

1. every contributing request is exact, **and**
2. the observation window is complete — monitoring did not start mid-session, no events were missed, no
   reconciliation failed, no subagent directory was unreadable.

`AggregateAccuracy::Exact` is reachable through exactly one constructor requiring proof of both. Twelve
exact requests plus one unavailable yields `MixedWithGaps`, and the UI renders
`847,231 tokens — 12 of 13 requests measured, 1 unmeasured`. There is no code path from that state to the
string "EXACT".

---

## 4. Capture levels

| Level | Mechanism | Fidelity | Status |
|---|---|---|---|
| 1 | Provider usage metadata relayed by the application (transcript / rollout files) | Exact | **MVP** |
| 2 | Application integration — structured logs, CLI JSON output, OTEL, session files | Exact to Calculated | MVP (files); OTEL post-MVP |
| 3 | Explicit local proxy the user configures their client to use | Exact (`ProtocolMetadata`) | Post-MVP |
| 4 | Tokenizer counting over text we legitimately hold | Calculated | Post-MVP, extension point only |

Level 1 is preferred and is sufficient for both agents in the MVP. Level 3 exists for generic
OpenAI-/Anthropic-compatible clients that write nothing to disk; it is opt-in by construction, since the
user must point their own client at it. Level 4 is deliberately unused for sources that already report
exact counts — a tokenizer estimate that disagrees with a provider-reported figure is strictly worse than
the figure.

---

## 5. Normalization: the two providers do not mean the same thing

This is the single most dangerous part of the system, because both vendors publish a field called
`input_tokens` and they mean different things.

- **Anthropic**: `input_tokens` **excludes** `cache_creation_input_tokens` and `cache_read_input_tokens`.
  The three are disjoint and billed at different rates.
- **OpenAI/Codex**: `cached_input_tokens` is a **subset of** `input_tokens`, and
  `reasoning_output_tokens` is a **subset of** `output_tokens`.

Treating either convention as universal silently corrupts both totals and cost. Charging Codex's
`input_tokens` in full *and* its `cached_input_tokens` again overstates that turn's cost by **145 %**.

The fix is a canonical type whose buckets are a **disjoint partition**, with private fields and no public
constructor:

```rust
pub struct TokenUsage {
    input_fresh: u64,               // full-rate input, disjoint from every cache bucket
    cache_read: u64,
    cache_write_5m: u64,            // split by TTL: the multipliers differ (1.25x vs 2x)
    cache_write_1h: u64,
    cache_write_unspecified: u64,
    output_total: u64,              // reasoning already included where the provider reports it
    reasoning: Option<u64>,         // None where unreported — never Some(0)
}
```

There is no field called `input_tokens`, so there is nothing for a future contributor to naively add
`cache_read` to. The only ways in are `from_anthropic` and `from_openai_delta`; the latter *cannot*
forget to subtract the cached portion, because subtraction is the only path that compiles. (It uses
`checked_sub` — an unchecked `u64` subtraction wraps to ~1.8×10¹⁹ in release builds, where the bug would
actually ship.)

**What the UI calls "Input"** is `input_fresh + cache_read + cache_write` for both providers, with the
breakdown one click away. Rendering the raw provider field instead would show `2` for a Claude request
that actually processed 54,497 input-side tokens, next to `16,210` for a Codex one — inviting the reader
to conclude the Claude request was thousands of times cheaper when it was in fact larger. On a screen
whose entire purpose is comparison, a per-provider definition of "Input" is not a nuance. It is a lie.

`reasoning: None` propagates as absence, never as zero. An aggregate mixing Claude and Codex requests
yields `Partial { value, known: 8, total: 20 }`, rendered as `1,234 · 8 of 20 requests report reasoning`.

---

## 6. Adapter architecture

```
crates/aum-adapters/
├─ src/lib.rs            UsageAdapter trait, capabilities, RawSignal
├─ src/claude_code/      transcript parser, dedup, subagents, retry chains
├─ src/codex/            rollout parser, cumulative reconciliation state machine
└─ src/claude_desktop/   detection only; reports Unavailable with a reason
```

```rust
#[async_trait]
pub trait UsageAdapter: Send + Sync + 'static {
    fn id(&self) -> AdapterId;
    fn meta(&self) -> AdapterMeta;

    async fn detect(&self) -> Detection;
    async fn probe_capabilities(&self, d: &Detection) -> CapabilitySet;
    async fn watch_roots(&self, d: &Detection) -> Vec<WatchRoot>;

    /// HOT PATH. Pure, synchronous, no I/O.
    fn parse_line(&self, ctx: &mut LineCtx, line: &[u8]) -> ParseOutcome;
    /// Cheap byte prefilter. Conservative: false positives fine, false negatives are data loss.
    fn is_candidate_line(&self, head: &[u8]) -> bool;

    fn launch_spec(&self, req: &LaunchRequest) -> Result<LaunchSpec, LaunchError>;
    async fn live_sessions(&self, d: &Detection) -> Vec<LiveSessionCandidate>;
}
```

`parse_line` being pure and synchronous is a deliberate structural choice: backfill can run on a blocking
pool with no async overhead, and parsers are unit-testable against golden fixtures with zero runtime.
Only the handful of lifecycle methods are async, so `#[async_trait]`'s boxing cost is irrelevant and we
keep `dyn` compatibility for free.

Adapters emit `RawSignal`s — `SessionOpened`, `TurnContext`, `Usage`, `ProviderCost`, `RateLimit`,
`Anomaly` — and never touch the database. Each is isolated, so a change in Claude Code's or Codex's
format breaks one parser and one set of golden tests, not the application.

### 6.1 Capabilities are discovered, not declared

The requirement that the capability matrix reflect reality rules out a hardcoded table.
`probe_capabilities` reads the tail of the few most recent transcripts through the *same* `parse_line`
and tallies which normalized fields real data actually populated, recording evidence:

```rust
CacheTtlBreakdown = Supported { evidence: "cache_creation.ephemeral_1h_input_tokens=23610" }
ReasoningTokens   = Unsupported { reason: "no reasoning field exists in .message.usage" }
PerRequestLatency = Unsupported { reason: "no latency field in transcripts; enable OTEL for this" }
```

`CapabilityState::Unknown` renders as `?`. It never renders as yes.

---

## 7. Attribution

> Attribution is by identity, never by heuristic. If identity is not established, usage is recorded
> against `task_id = NULL` and surfaced in an **Unattributed** view. It is never guessed into a task.

Working directory is explicitly **not** a binding. On the machine this was designed against, three Claude
Code sessions were live simultaneously in the same `cwd`, and that one project directory held 77
transcripts. `cwd → session` is one-to-many; it is used only to filter the candidate list in the attach
UI, where a human makes the choice.

**Launched mode** (the monitor spawns the agent) gives exact attribution by construction:

- *Claude Code* — we generate the session UUID and pass `--session-id <uuid>`. Subagent and workflow
  transcripts carry the same parent `sessionId`, so they attribute automatically.
- *Codex* — we run `codex exec --json` and read the event stream from **our own child's stdout**. Same
  events, same parser, no filesystem correlation required.

**Attached mode** binds an already-running agent: Claude via `~/.claude/sessions/<pid>.json`, validated
against the OS process start time to defeat PID reuse. Ambiguity is surfaced, never resolved by picking.

The guarantee that twenty concurrent tasks cannot cross-contaminate is structural, not diligent:

```sql
CREATE TABLE task_binding (
  session_id TEXT PRIMARY KEY,   -- a session belongs to at most ONE task, enforced by SQLite
  task_id    TEXT NOT NULL REFERENCES task(id) ON DELETE CASCADE,
  method     TEXT NOT NULL,      -- launched_pinned | pid_session_file | session_id_exact | user_assigned
  evidence   TEXT NOT NULL       -- JSON proof of this binding
);
```

The ingest write path performs exactly one indexed equality lookup, `session_id → task_id`. There is no
fuzzy match, no scoring and no nearest-neighbour anywhere in it. A conservation check runs every tick:

```
Σ(attributed to tasks) + Σ(unattributed) == Σ(all observed usage)
```

which is what makes the Unattributed bucket load-bearing rather than a dumping ground — totals still
reconcile, and the user can see exactly what the app declined to guess about.

---

## 8. Benchmark → Task → Session → Request

```
Benchmark            a named comparison ("Implement JWT authentication")
  └── Task           one competitor's attempt (agent + command + working dir)
        └── Binding  session_id → task_id   (PRIMARY KEY: at most one task per session)
              └── AgentSession    a provider session observed by an adapter
                    └── AiRequest one logical model turn
                          ├── TokenUsage        one row per measurement source
                          └── CostCalculation   one row per cost basis
```

A `Task` is the unit of measurement and may exist without a benchmark (ad-hoc monitoring). A
`Benchmark` groups tasks for comparison and carries the environment metadata that makes a run
reproducible: OS, CPU, RAM, app version, adapter versions, model identifiers, pricing version, FX
version, start and end.

`TokenUsage` is a separate table rather than columns on `AiRequest` because Claude Code can report the
same request three ways — transcript, OTEL, and `stream-json` — and comparing them is precisely the
accuracy signal this application exists to surface. A denormalized copy of the winning source lives on
`ai_request` for the dashboard's hot path, chosen by explicit precedence and written in the same
transaction.

### 8.1 Idempotency keys

| Provider | Key | Merge |
|---|---|---|
| Claude Code | `(session_id, request_id, message_id)` | component-wise **MAX** |
| Codex | `(session_id, event_ordinal, kind)` | insert once |

Claude Code writes one API response as *N* JSONL lines — one per parallel tool call — each repeating the
entire `usage` object. Up to 21 lines for a single response were observed. Summing lines over-counts by
**2.1×–3.5×**. Where the repeated copies differ, the values are monotonically non-decreasing (an early
line is flushed mid-stream), so component-wise `MAX` is both correct and order-independent — which is
what makes re-ingest after a crash provably safe.

Codex reports cumulative totals, so per-request rows come from *watermark advance*. A replayed event
carries an identical cumulative vector, so it collapses to a no-op and contributes zero. The cumulative
ledger is authoritative; `last_token_usage` is a cross-check only. It also does **not** reset on context
compaction — an implementation that re-baselines there double-counts the entire remainder of the session.

---

## 9. Cost

Three quantities, three fields, never one column called "Cost":

| Quantity | Meaning | On this machine |
|---|---|---|
| **API-equivalent** | What this usage would cost on pay-as-you-go, from our versioned price table. | Computed |
| **Provider-reported** | The agent's own cost figure (`total_cost_usd`, `claude_code.cost.usage`). | Claude Code only |
| **Actual billed** | What the user was actually charged. | **Unavailable** — both agents are subscription-billed |

Presenting an API-equivalent figure as though the user was charged it would be the most consequential
dishonesty this application could commit, so `<Money>` requires an explicit `kind` prop with no default;
there is no way to render a currency amount without declaring which of the three it is.

Prices are versioned and **append-only**. A user edit creates a new version and closes the previous one,
so a benchmark run in March still displays March's numbers; recalculation is an explicit, audited action.
An unknown model yields `Unavailable { NoPricingForModel }` and a prompt to set a price — never a
substituted "similar" model's rate. This matters immediately: `claude-opus-5`, `claude-fable-5` and
`gpt-5.6-sol` all appear in the real corpus and in no public price list.

Money is `rust_decimal::Decimal` in process, integer nano-USD at rest, and a **decimal string** on the
wire — a JSON number would be silently mangled by JavaScript. Costs are summed exactly and rounded once
at display; rounding per request and then summing drifts the total and forfeits any claim that it is
exact.

Divergence beyond 2 % between our figure and the provider's raises a `PricingDrift` anomaly. That is the
application self-testing its own price table.

---

## 10. Latency

Neither transcript source records model latency or time-to-first-token. What is legitimately derivable:

| Metric | Transcript only | + OTEL | + Proxy |
|---|---|---|---|
| Time to first token | **Unavailable** | **Unavailable** | Exact |
| Request latency | **Unavailable** | Calculated (turn e2e, includes tool time) | Exact |
| avg / median / p95 latency | **Unavailable** | Calculated | Exact |
| Output tokens/sec | **Unavailable** | Calculated, caveated | Exact |
| Tool execution time | Exact (`durationMs`) | Exact | Exact |

The trap worth naming: `output_tokens ÷ (gap between message timestamps)` produces a plausible,
well-scaled, entirely wrong tokens-per-second figure. That denominator contains tool execution, retry
backoff, queueing and user think time. Because one response is written as several lines spanning seconds,
even the numerator's timestamp is ambiguous — we take the *earliest* and record the spread as evidence.
The inter-message wall-clock type therefore has no conversion to a rate; the metric is `Unavailable`
until a capture level that can measure it is enabled.

Benchmark cells that are Unavailable render as such, with a chip naming what would make them available.
Sorting or ranking on a column that is Unavailable for either side is disabled — sorting a column with a
missing side is exactly the mechanism by which a UI invents a winner.

---

## 11. Process architecture

```
┌──────────────────────────── Electron ────────────────────────────┐
│  main      lifecycle, windows, sidecar supervision. No domain     │
│            knowledge. ~250 lines.                                 │
│  preload   contextBridge only, 8 control-plane channels           │
│  renderer  the entire product. Talks HTTP/SSE to the sidecar.     │
└───────────────────────────────────────────────────────────────────┘
                │ spawn + one-line stdout handshake {port, pid, contract_version}
                │ token passed via env, never argv (argv is world-readable)
                ▼
┌──────────────────────── Rust sidecar ────────────────────────────┐
│  aum-server   axum on 127.0.0.1:0, JSON + SSE, bearer auth        │
│  aum-engine   task supervisor, bindings, adapter lifecycle, bus   │
│  aum-adapters claude_code · codex · claude_desktop                │
│  aum-ingest   tailer: cursors, partial lines, prefilter, batching │
│  aum-db       SQLite (WAL), migrations, single write actor        │
│  aum-domain   TokenUsage, Money, MeasurementSource  (pure)        │
│  aum-pricing  versioned prices, FX, decimal cost engine           │
└───────────────────────────────────────────────────────────────────┘
```

**Business logic lives in Rust.** Electron starts a binary, reads one line from its stdout, and kills it
on quit. Usage data does not cross Electron IPC: routing it through the main process would require a
marshalling layer per DTO, which would quietly become a second, drifting copy of the domain model — and
would serialize hundreds of structured clones per second behind menu handling and window events, in an
application whose whole purpose is to not perturb what it measures.

The replaceability boundary is therefore exactly three things: a one-line stdout handshake, the HTTP
surface described by `packages/api-contract/openapi.json`, and an SSE stream honouring `Last-Event-ID`. A
Go, C# or Python reimplementation satisfying those needs no changes anywhere in the desktop app. The
`tools/fake-sidecar` Bun script exists partly to prove this continuously: if the app runs unmodified
against a TypeScript backend, it will run against any of them.

### 11.1 Local transport is still a security boundary

Any local process — and any web page in any browser — can reach `127.0.0.1`. Four layers, all mandatory:
an ephemeral port; a 256-bit bearer token compared in constant time; a `Host` header that must equal
`127.0.0.1:<port>` (this is what defeats DNS rebinding, which cannot forge `Host`); and an exact `Origin`
allowlist with no wildcard. The SSE stream is read with `fetch` + `ReadableStream` rather than
`EventSource`, because `EventSource` cannot set headers and putting the token in a query string leaks it
into logs.

### 11.2 Backpressure

Adapters → pipeline is a **bounded, lossless** channel: if the database is slow, ingest slows down and
never drops tokens. Pipeline → SSE is a **bounded, lossy** broadcast: a slow browser must never stall
ingest, and a lagged client receives a `Resync` telling it to re-fetch. Live traffic is preferred over
historical backfill by a biased select.

Two rules keep the frontend both simple and correct: every event type has a corresponding GET that
returns the same state, so a client that misses events is always one request from correct; and the
per-second snapshot carries **absolute cumulative totals**, so the UI never accumulates deltas and a
dropped connection cannot permanently corrupt a running total.

---

## 12. Privacy

- No account, no telemetry, no analytics, no cloud database, no remote backend.
- Storage is a single local SQLite file under the OS application-support directory.
- **Content storage is off by default** — prompts, responses and tool input/output are not recorded.
  Metadata only: timestamps, provider, model, token counts, cost, duration, status, task, application.
  Enabling content storage carries an explicit warning that AI conversations routinely contain secrets.
- Exports never include content unless content storage is explicitly enabled.
- The renderer is structurally incapable of reaching the network: a CSP plus a request filter in the
  Electron main process cancels any renderer request that is not the local sidecar, and the count of
  blocked attempts is shown in Settings.
- The only outbound network the application makes at all is exchange-rate and pricing updates, from the
  sidecar, each individually disableable. Offline, the last known FX rate is used and its age is
  displayed — an old rate is never presented as current.
- Test fixtures derived from real transcripts are redacted before they are written, preserving every
  number and structural field while replacing all free text.

---

## 13. Documents

| File | Contents |
|---|---|
| `docs/architecture.md` | this document |
| `docs/capture-methods.md` | the four capture levels in detail, and per-adapter fidelity |
| `docs/privacy.md` | what is stored, where, and what leaves the machine |
| `docs/benchmarking.md` | running comparisons, metrics, and their comparability caveats |
| `docs/adapter-development.md` | adding support for a new AI application |
| `docs/pricing.md` | the pricing model, versioning, and currency handling |
