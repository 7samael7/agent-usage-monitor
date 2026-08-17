# Capture methods

How this application obtains token counts, what each method can and cannot
establish, and why some things are simply not knowable.

Every number carries a `measurementSource`, and the table at the end says which
method produces which. Nothing here is aspirational: the capability matrix in
the Applications screen is generated at runtime by running the real parsers over
each application's own files, so if this document and that screen ever disagree,
the screen is right.

---

## The four levels

| Level | Method | Fidelity | Requires |
|---|---|---|---|
| 1 | Provider usage metadata the application writes to disk | **Exact** | nothing |
| 2 | Application integration — CLI JSON output, OpenTelemetry | Exact to Calculated | configuration |
| 3 | A local proxy the user points their own client at | **Exact** | explicit opt-in |
| 4 | Tokenizer counting over text we legitimately hold | Calculated | text we already have |

Level 1 is preferred and is sufficient for both agents supported today. Level 3
exists for clients that write nothing down. Level 4 is deliberately unused where
a provider already reports counts, because a tokenizer estimate that disagrees
with a provider-reported figure is strictly worse than the figure.

---

## Level 1 — reading what the application already writes

Both supported agents record the provider's own usage object to a local file as
they work. This is the best possible source short of being the provider: the
numbers are authored by the serving infrastructure and relayed without
recomputation.

### Claude Code

```
~/.claude/projects/<cwd-with-slashes-replaced-by-dashes>/<session-uuid>.jsonl
```

Appended in real time, one line per message. `type: "assistant"` lines carry
`.message.usage`, which is Anthropic's Messages API usage object verbatim —
recognisable as such because it includes `service_tier` and `inference_geo`,
fields the CLI has no way to compute.

Three things make reading it non-obvious, all measured against a real corpus of
463 files:

**One response is written as many lines.** A response containing several
parallel tool calls becomes several JSONL lines, each repeating the *entire*
usage object, with different uuids and timestamps spanning seconds. Across the
whole corpus, 53,725 usage-bearing lines describe 22,184 distinct requests.
Summing lines reports 59.6M output tokens where the truth is 24.5M — an
inflation of **2.44×**. Deduplication by `(requestId, message.id)` is not an
optimisation; it is the difference between right and wrong.

**Sub-agent work is in other files.** `<session>/subagents/**.jsonl` accounts for
72% of the files and 34% of the requests. Those lines carry the *parent*
session id, so attributing on the per-line `sessionId` picks them up
automatically — while attributing on file path loses all of it.

**Some lines are not requests.** `model: "<synthetic>"` marks a terminal API
failure with zero usage; counting it corrupts request counts and failure rates.
`compact_boundary` reports context-window sizes near a million tokens, which
were never a request.

Not present: **reasoning-token counts** and **request latency**. Claude Code
emits `thinking` content blocks but no count for them. Both are permanent
`Unavailable`s at this level, not gaps waiting to be filled.

### Codex

```
~/.codex/sessions/YYYY/MM/DD/rollout-<timestamp>-<uuid>.jsonl
```

After every turn Codex writes a `token_count` event carrying two figures:
`total_token_usage`, a running total for the session, and `last_token_usage`,
the most recent call. **Neither is correct on its own**, and they err in
opposite directions. Across all 21,784 usage events on the reference machine:

| transition | count | what it is |
|---|---:|---|
| cumulative advanced | 21,341 | an ordinary turn |
| advanced by zero, `last` unchanged | 181 | the same event emitted twice |
| advanced by zero, `last` changed | 203 | a call billed but left off the ledger |
| cumulative went backwards | 1 | the session counter was rebased |

Summing `last_token_usage` over-counts by ~0.89% because it counts the
duplicates. Trusting the final `total_token_usage` under-counts by ~0.98%
because it misses the compaction calls — real API calls, really billed, that the
provider excludes from its own running total. The cumulative counter is
authoritative for ordinary turns, `last` is the cross-check, and the
disagreement between them is what identifies the off-ledger calls.

Codex **does** report reasoning tokens, which Claude Code does not. It reports
no per-request latency.

One further wrinkle: those off-ledger calls report a total with `input_tokens`
and `output_tokens` both zero. The tokens are real — 3,082,662 of them across
the reference machine — but the provider does not say which side they fall on.
They are counted in an `unclassified` bucket and **priced nowhere**, because
input and output rates differ by roughly eight times and attributing them to
either side would be a material error rather than a rounding one.

### Claude Desktop

One number, and no structure. It writes two quantitative files.

```
~/Library/Application Support/Claude/plan-usage-history.json
```

samples plan-limit percentages every few minutes — `{"t":…,"u":{"fh":0,"sd":13}}`,
the five-hour and seven-day windows. There is no defensible transform from "13%
of a seven-day window" to a token count.

```
~/Library/Application Support/Claude/buddy-tokens.json
{ "tokens-today": { "date": "2026-08-13", "tokens": 1119656 } }
```

is a real running token count. It is read, recorded and shown — and every token
capability still reports `Unsupported`, because each one asks something this
number cannot answer:

- **no model**, so it cannot be priced: input and output rates differ by roughly
  five times and the split is not given either;
- **no request boundary**, so it cannot be a request count, a latency, or a
  per-turn anything;
- **no session id**, so it can never join anything — and attribution in this
  application is by identity, never by timing or coincidence;
- **today only**: the counter resets at midnight and the previous day is
  discarded, so the history exists only because the monitor samples it.

It is therefore carried on the adapter as its own daily total, with a sentence
stating its scope attached to the value, and stored in `desktop_daily_tokens` —
a table none of the request aggregates read. That separation is structural rather
than careful: those queries run over `ai_request`, and this number is not in it,
so there is no query that could accidentally fold a whole-application daily
figure into a per-model or per-day total.

Classified `ApplicationTelemetry` — Claude Desktop computed this itself, and
nothing in the file indicates the provider's own per-request usage — so it
displays as **Calculated**, never Exact.

---

## Level 2 — application integration

Both agents can be configured to export OpenTelemetry metrics, which would add
per-request latency and, for Claude Code, a cost figure the application computes
itself. That figure is `ApplicationTelemetry`, not `ProviderReported`: the
provider never sent a dollar amount for subscription usage, so the CLI derived
it. This level is designed for but not yet implemented.

---

## Level 3 — the local proxy

For clients that write nothing to disk, the user can point their own application
at a local proxy, which reads the usage field out of the protocol as it passes.
That is `ProtocolMetadata` — provider-authored, observed at the transport — and
it is exact.

This is consent-based observation, not interception: the user configures their
own client, knowingly, and can stop at any time. Not yet implemented.

---

## Why arbitrary desktop traffic cannot be measured

A reasonable question is why the monitor does not simply watch the network. The
answer sets the honest ceiling on what this application can claim.

**Observing that traffic exists is not observing what it contains.** A closed
desktop client talks to its provider over TLS. Reading the bodies would require
terminating that connection — installing a root certificate, re-signing the
traffic, impersonating the provider. That is a man-in-the-middle attack on
another application, and this project does not do it:

- it requires modifying another application's trust store;
- it exposes the user's authentication tokens and full conversation content to a
  third process;
- it breaks certificate pinning in ways indistinguishable from an attack;
- it decrypts unrelated HTTPS traffic as collateral.

**And it would not work anyway.** Token accounting happens server-side, against
the fully assembled prompt — including the system prompt, tool schemas, and any
server-side injections the client never sees. Counting the visible request body
locally reproduces none of that. It yields a *calculated* number, structurally
lower than the truth, and calling it exact would be a lie with a plausible
shape.

So the ceiling is set by what an application chooses to tell us. Detecting a
running process proves a process is running. It proves nothing about tokens, and
this application will not pretend otherwise.

---

## What each method yields

| Source | Measurement source | Displayed as |
|---|---|---|
| Claude Code transcript `.message.usage` | `ProviderReported` | **Exact** |
| Claude Code sub-agent transcripts | `ProviderReported` | **Exact** |
| Claude Code `<synthetic>` failure line | — | **Unavailable** |
| Claude Code `compact_boundary` | not a measurement | n/a |
| Codex cumulative delta | `ProviderReported` | **Exact** |
| Codex off-ledger compaction call | `ProviderReported` | **Exact**, unpriceable |
| Codex `rate_limits` | `ApplicationTelemetry` | not tokens |
| OpenTelemetry token counters | `ProviderReported` | **Exact** |
| OpenTelemetry cost counter | `ApplicationTelemetry` | **Calculated** |
| Proxy-observed protocol usage | `ProtocolMetadata` | **Exact** |
| Local tokenizer over text we hold | `TokenizerCalculated` | **Calculated** |
| Claude Desktop plan-limit sampler | — | **Unavailable** |
| Claude Desktop daily token counter | `ApplicationTelemetry` | **Calculated**, whole-application daily total only |

The last four rows describe how those sources *would* be classified. The
OpenTelemetry receiver, the proxy and tokenizer counting are not implemented in
this backend; the rows are here because the classification is part of the wire
contract, and a different backend implementing any of them should produce these
values. Tokenizer counting in particular is unlikely ever to be worth adding
here: an estimate helps only where nothing better exists, and both agents
already report provider-authored counts, while the one source that reports
nothing also exposes no text to count.

A total is only ever labelled exact when *every* contributing request was
measured **and** the observation window itself is complete. Thirteen perfectly
measured requests observed from halfway through a session is still not an exact
total, and the application says so.
