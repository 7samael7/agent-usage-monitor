# Comparing agents

`aum models` and `aum overview` put Claude Code and Codex in the same table.
This page is about which of those columns may honestly be compared, because
several of them cannot.

> This document used to describe a benchmark runner: the monitor launched two
> agents at the same prompt, bound each session before it produced any usage,
> and compared the results. That feature is gone — the monitor reads history now
> and starts nothing. What survived is the part that was never about launching:
> the reasons two numbers side by side can mislead.

## Which columns are comparable

| Metric | Claude Code | Codex | Comparable? |
|---|---|---|---|
| Input, output, cache read/write | yes | yes | yes |
| Total tokens | yes | yes | yes |
| Reasoning tokens | **not reported at all** | reported per request | **no** — unavailable, not zero |
| Requests | yes | yes, a floor if events were missed | mostly |
| Failures | yes | yes | yes |
| Retries | yes | **not exposed** | **no** — unavailable, not zero |
| Latency, TTFT, tokens/sec | **not in transcripts** | **not in rollouts** | neither, and not derived |
| API-equivalent cost | yes, when priced | yes, when priced | yes |
| Actual billed cost | **unknown** (subscription) | **unknown** (subscription) | neither |

**"Input" means fresh input plus cache reads plus cache writes**, for both
agents. That is the only input quantity that means the same thing on both sides:
a real Claude request reports `input_tokens: 2` while having processed 54,497
input-side tokens, and putting that raw field beside a Codex figure would invite
exactly the wrong conclusion. This is why the tables show *input side* rather
than *input*.

**Reasoning tokens are the sharpest case.** Claude Code emits `thinking` content
blocks and no count for them; Codex reports `reasoning_output_tokens` per
request. A total across both agents is therefore a floor and is rendered `≥`, and
the per-agent figure for Claude Code is `—`, never `0`. Reading that dash as zero
would say Claude Code did no reasoning, which is the opposite of true.

Even Codex's own figure is a floor: on the corpus this was written against,
21,408 of 21,611 Codex requests carried a reasoning count. The 203 that did not
are the reason the number keeps its `≥`.

## Copilot, when it is measurable at all

With its OpenTelemetry export on, Copilot's counts are provider-reported and
compare directly with the other two. Two caveats belong beside any such
comparison:

- **The history is not comparable in length.** Copilot recorded nothing before
  the export was enabled, so its totals start from that moment while the others
  go back as far as their transcripts. A side-by-side of all-time figures
  compares a few days against a few months.
- **Its cache accounting has one bucket, not three.** Copilot reports a cache
  write with no TTL, so it lands in the unspecified bucket and is priced at the
  cheaper tier — a floor. Claude Code splits 5-minute from 1-hour writes, which
  price roughly 1.25× and 2× the input rate, and that split is most of what a
  long Claude session costs.

## What is deliberately not offered

**Latency and tokens per second.** The tempting substitute is the gap between
message timestamps. That number is plausible, well-scaled and wrong: the interval
contains tool execution, retry backoff, queueing and user think time, and because
one response is written across several lines seconds apart, even its endpoints
are ambiguous. It is not computed anywhere in this codebase, and there is no
column for it.

**A winner.** The application measures consumption. Whether the cheaper run
produced better work is a different question, and one it does not attempt to
answer.

**Sorting on a column that is unavailable for one side.** Ranking two agents by a
metric only one of them reports is precisely the mechanism by which an interface
invents a result.

## Reading a cost comparison

Costs are per-model and then summed, never a bucket's blended tokens at one rate:
the models in play differ by up to 10× per token, so a day spanning two of them
costs differently from that day's total at either single rate.

Both agents here are subscription-billed, so every cost figure is
API-equivalent — what this usage *would* have cost on pay-as-you-go — and is
always shown beside `actually billed —  subscription, not billed per token`. A
comparison of two API-equivalent figures is a comparison of consumption, not of
money that changed hands.

Cost rows pin the pricing version they used, so usage recorded in March still
shows March's rates in August. Recalculating is an explicit action, never a side
effect of editing a price.
