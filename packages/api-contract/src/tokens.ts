/** Mirrors `aum_contract::tokens`. */

/**
 * Token counts in **mutually exclusive** buckets.
 *
 * This is what makes a stacked chart honest for both providers at once.
 * Stacking the providers' raw fields instead would double-count Codex's
 * `cached_input_tokens` (a subset of its input) and `reasoning_output_tokens`
 * (a subset of its output), producing a plausible chart that is ~20% too tall
 * for Codex only — a bug that looks like nothing at all.
 */
export interface TokenBands {
  input_fresh: number
  cache_read: number
  cache_write_5m: number
  cache_write_1h: number
  cache_write_unspecified: number
  output_total: number
  /** `null` when the provider does not report reasoning tokens — never 0. */
  reasoning: number | null
  /**
   * Tokens the provider counted but did not classify as input or output.
   *
   * Codex's compaction calls report a total of ~13,000 with `input_tokens: 0`
   * and `output_tokens: 0`. They are real and are counted, but they cannot be
   * priced: input and output rates differ by roughly eight times, so splitting
   * them by guess would be a material error. Show them; do not cost them.
   */
  unclassified: number
}

export function cacheWriteTotal(b: TokenBands): number {
  return b.cache_write_5m + b.cache_write_1h + b.cache_write_unspecified
}

/**
 * What the UI labels "Input".
 *
 * The only cross-provider-comparable input quantity. A real Claude request in
 * the corpus reports `input_tokens: 2` while having processed 54,497 input-side
 * tokens; rendering the raw field beside a Codex figure would invite exactly
 * the wrong conclusion on the screen whose entire purpose is comparison.
 */
export function inputSideTotal(b: TokenBands): number {
  return b.input_fresh + b.cache_read + cacheWriteTotal(b)
}

export function grandTotal(b: TokenBands): number {
  return inputSideTotal(b) + b.output_total + b.unclassified
}

/** `null` rather than 0% when there was no input at all. */
export function cacheHitRate(b: TokenBands): number | null {
  const denom = inputSideTotal(b)
  return denom === 0 ? null : b.cache_read / denom
}

export const EMPTY_BANDS: TokenBands = {
  input_fresh: 0,
  cache_read: 0,
  cache_write_5m: 0,
  cache_write_1h: 0,
  cache_write_unspecified: 0,
  output_total: 0,
  reasoning: null,
  unclassified: 0,
}
