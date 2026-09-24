# Pricing

## Three quantities, never one

| | What it means | Typical state here |
|---|---|---|
| **API-equivalent** | What this usage would cost on pay-as-you-go, from our price table | computed, when the model has a price |
| **Provider-reported** | The agent's own cost figure | **unavailable** — neither agent writes one |
| **Actual billed** | What you were really charged | **unknown** under a subscription |

These are never summed and never collapsed into a column called "cost". The row
labelled `API-equivalent` is always followed by `actually billed —  subscription,
not billed per token`, on every screen and in every export, so the number is
never alone on the page with a currency symbol and no qualifier.

Presenting an API-equivalent figure as though you were charged it would be the
most consequential dishonesty this application could commit. Both agents on a
typical machine are subscription-billed, so "actual billed" is genuinely
unknowable and says so.

## Arithmetic

**No floats.** `rust_decimal` end to end. A cent cannot be represented in binary
floating point, and per-request amounts are frequently in the 1e-7 range and
summed over tens of thousands of requests. Money is stored as integer nano-units
and serialised as a **decimal string** — `Money`'s deserializer *rejects* a JSON
number rather than accepting whatever a float parser made of it.

**Round once, at the end.** Each request is costed exactly, the exact values are
summed, and the result is rounded only for display. Rounding per request and
then summing drifts; a test costs ten thousand requests that each round to
nothing and asserts the total is not zero.

## Cache writes price by time-to-live

A one-hour cache write costs roughly twice the input rate where a five-minute
write costs about 1.25×, and real Claude Code sessions are dominated by the hour
tier. Collapsing them understates a real request by **24%**.

A write whose TTL the provider did not report goes to an explicitly unpriceable
bucket and is costed at the cheaper tier, so the figure is a floor rather than a
guess that could overstate.

## What is never done

**Substituting a similar model's price.** Lookup is exact. Prefix or family
matching would quietly price `claude-opus-5` at `claude-opus-4`'s rate and
produce a confident total wrong by an unknown factor.

**Defaulting an unknown model to zero.** A model with no entry yields
`Unavailable` with the model name and a prompt to enter a rate. This is not
hypothetical: a new model appears every few weeks, and for a while it postdates
every price list anyone has read. Defaulting those to zero would show the
heaviest sessions on the machine as free — and they are usually the heaviest
precisely because the model is new.

## Where a rate comes from

Two places, and a rate says which:

- **the seed**, `crates/aum-pricing/seed/models.json`, compiled into the binary.
  Every entry carries the page it was read from and the day it was read. This is
  why a fresh install prices its history without being told anything. A model
  whose price has changed keeps every version, each from the day it took effect.
- **the local database**, from `aum price`. It does not travel between machines,
  which is worth knowing before wondering why a second machine shows
  `not priced`: either it has an older build, or the model is newer than it.

Seeded rates go stale — providers change prices, and nothing here fetches an
update — so an entry is the correction mechanism rather than an edge case. It
takes over from the moment you make it, as described under
[Versioning](#versioning).

**Pricing tokens the provider did not classify.** Codex's compaction calls
report a total with no input/output split. Those tokens are counted and priced
nowhere, because the two rates differ by roughly eight times.

**Double-counting.** The cost function accepts only a normalized `TokenUsage`,
whose buckets are disjoint by construction. Charging cached input twice
(+145%) or billing reasoning on top of output (+4%) are not expressible.

## Versioning

Prices are **append-only**. A new figure adds a version with its own
`effective_from`; nothing is edited in place, and the old version stays.

**Usage is charged the rate in force when it happened.** Every version of a
model, shipped or entered, sits on one timeline, and each applies from its
`effective_from` until the next one begins. A request made in March keeps
March's rate after an August change, and last month's total does not move
because a provider started a promotion this month. Nothing is stored to make
that true: a cost is computed when it is shown, from the version whose period
contains the request. Usage is summed per model per day before it is priced, so
the sums are cut wherever a price changes, and a day a price changed in is
priced in two parts.

Three rules decide where a version's period starts and ends:

- **A rate you enter begins when you enter it.** What happened before keeps the
  rate that was in force then.
- **Usage older than every version takes the earliest.** That is what lets a
  rate entered today price a model that has been running unpriced for weeks:
  that usage had no rate to keep, so there is nothing for the new one to
  overwrite.
- **At the same instant, your entry beats a shipped version**, because someone
  who has typed in a rate knows something the seed does not. A shipped version
  that begins *after* your entry takes over from its start: it records a price
  change your entry could not have known about, so a release that catches up
  with a price cut applies it without anyone re-typing anything.

```bash
aum price gpt-5.6-terra --input 2.00 --output 12.00 --note "openai pricing page, 2026-08-17"
```

The rate printed beside a model in `aum models` and `aum overview` is the one
that priced its latest usage in the range. `(+1 earlier)` after it says the
range spans a price change, so no single rate reproduces the cost beside it;
`--json` lists every rate used under `earlier_rates`.

`--cache-read` defaults to the input rate, which is both providers' documented
behaviour. It does **not** default to zero: pricing cache reads as free would
understate a long session by most of its total.

## Currency

USD is canonical, because that is what providers publish. EUR and CZK are
presentation, produced by applying a dated rate.

Conversion **downgrades certainty**. A converted amount is at best *calculated*,
never exact — it depends on a rate that was true at a moment and is not now. An
amount converted with a rate a week or more old becomes *estimated*, and the
output states the rate's age and date rather than merely flagging it.

Nothing fetches rates. You enter one with `aum fx EUR 0.92` and it is stored
with the date you entered it, which is exactly why the staleness rule exists: a
rate typed in last month is still sitting there, and presenting a twelve-day-old
rate as current would be a small lie compounding on top of an already-approximate
cost.
