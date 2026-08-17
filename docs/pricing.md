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
hypothetical: every model a current machine actually runs — `claude-opus-5`,
`claude-fable-5`, `gpt-5.6-sol`, `gpt-5.6-terra` — postdates every published
price list. Defaulting them to zero would show the heaviest sessions on the
machine as free.

**Pricing tokens the provider did not classify.** Codex's compaction calls
report a total with no input/output split. Those tokens are counted and priced
nowhere, because the two rates differ by roughly eight times.

**Double-counting.** The cost function accepts only a normalized `TokenUsage`,
whose buckets are disjoint by construction. Charging cached input twice
(+145%) or billing reasoning on top of output (+4%) are not expressible.

## Versioning

Prices are **append-only**. Correcting one adds a version with a new
`effective_from` and closes the previous one; nothing is edited in place. Each
cost row pins the pricing version it used, so usage recorded in March still
displays March's rates in August, and recalculating is an explicit action.

A rate you entered yourself beats a seeded one at equal specificity, because
someone who has typed in a rate knows something the seed does not:

```bash
aum price gpt-5.6-terra --input 2.00 --output 12.00 --note "openai pricing page, 2026-08-17"
```

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
