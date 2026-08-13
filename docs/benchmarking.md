# Benchmarking

Running two agents at the same job and comparing what each one spent.

## How a benchmark is structured

```
Benchmark            a named comparison ("Implement JWT authentication")
  └── Task           one competitor's attempt (agent + prompt + working dir)
        └── Binding  session -> task, enforced one-to-one by the database
              └── Requests, usage, and cost
```

A task is the unit of measurement and can exist without a benchmark. Start one
task, then start another with the same prompt and a different agent; the
comparison groups whatever has run.

## Why attribution is trustworthy

When the monitor launches the agent, it fixes the identity that usage will
arrive under *before* any usage exists:

- **Claude Code** — the monitor generates the session UUID and passes
  `--session-id`, and writes the binding before starting the process. The first
  request it makes is already attributed. Sub-agent transcripts carry the same
  parent session id, so they attribute for free.
- **Codex** — no equivalent flag exists, so the monitor reads
  `codex exec --json` from its own child's pipe and binds when the stream
  announces its id. Because the pipe belongs to a process we spawned, that is a
  fact rather than an inference.

`task_binding.session_id` is a primary key, so a session belongs to at most one
task and the database enforces it. Twenty concurrent tasks cannot contaminate
each other, and the ingest path resolves attribution with a single indexed
equality lookup — there is no scoring, nearest-match, or time-window heuristic
anywhere in it.

Working directory is **not** an identity. Three Claude Code sessions were
running in one directory when these formats were investigated, so two tasks may
safely share a directory and a task is never inferred from one.

Usage that cannot be attributed is recorded against no task and shown in the
unattributed view. It is never guessed into a task, and never dropped.

## Which columns are comparable

Not all of them, and the interface says which.

| Metric | Claude Code | Codex | Comparable? |
|---|---|---|---|
| Input, output, cache read/write | yes | yes | yes |
| Total tokens | yes | yes | yes |
| Reasoning tokens | **not reported** | yes | **no** — shown as unavailable, not zero |
| Requests | yes | yes, floor if events were missed | mostly |
| Failures | yes | yes | yes |
| Retries | yes | **not exposed** | **no** — unavailable, not zero |
| Latency, TTFT, tokens/sec | **not in transcripts** | **not in rollouts** | needs the proxy or OTEL |
| API-equivalent cost | yes, when priced | yes, when priced | yes |
| Actual billed cost | **unknown** (subscription) | **unknown** (subscription) | neither |

"Input" means fresh input plus cache reads plus cache writes, for both agents.
That is the only input quantity that means the same thing on both sides: a real
Claude request reports `input_tokens: 2` while having processed 54,497
input-side tokens, and putting that raw field beside a Codex figure would invite
exactly the wrong conclusion.

## What is deliberately not offered

**Latency and tokens per second, from transcripts.** The tempting substitute is
the gap between message timestamps. That number is plausible, well-scaled and
wrong: the interval contains tool execution, retry backoff, queueing and user
think time, and because one response is written across several lines seconds
apart, even its endpoints are ambiguous. It is not computed anywhere in this
codebase.

**A winner.** The application measures consumption. Whether the cheaper run
produced better work is a different question, and one it does not attempt to
answer. There is an extension point for scoring, deliberately empty.

## Reproducibility

A benchmark records the environment it ran in — OS, CPU, memory, application and
adapter versions, models, pricing version, exchange-rate version — so a
comparison stays interpretable later. Cost rows pin the pricing version used, so
a run from March still shows March's numbers in August. Recalculating is an
explicit action, never a side effect of editing a price.
