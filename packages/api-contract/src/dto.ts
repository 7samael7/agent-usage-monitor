/** Mirrors `aum_contract::dto`. */

import type { Measured } from './measurement'
import type { TokenBands } from './tokens'

export interface HealthResponse {
  status: string
  uptime_ms: number
}

export interface MetaResponse {
  contract_version: string
  impl_name: string
  impl_version: string
  stream_epoch: string
  capabilities: string[]
}

export type TaskStatus = 'pending' | 'running' | 'completed' | 'failed' | 'stopped'

export type TaskBinding =
  | { mode: 'launched_pinned'; session_id: string }
  | { mode: 'launched_stdout'; pid: number }
  | { mode: 'attached_pid_session_file'; pid: number; session_id: string }
  | { mode: 'session_id_exact'; session_id: string }
  | { mode: 'unbound' }

export interface TaskSummary {
  id: string
  benchmark_id: string | null
  name: string
  adapter_id: string
  status: TaskStatus
  binding: TaskBinding
  working_dir: string | null
  model_id: string | null
  started_at: string | null
  ended_at: string | null
}

export interface RequestCounts {
  succeeded: number
  failed: number
  /** `null` where the agent does not expose retries at all. Not 0. */
  retries: number | null
  /** Counts are a floor; render with `≥`. */
  is_lower_bound: boolean
}

export type Currency = 'USD' | 'EUR' | 'CZK'

/** A decimal amount as a string. Never parse this into a float for arithmetic. */
export type Money = string

export interface CostBreakdown {
  currency: Currency
  api_equivalent: Measured<Money>
  provider_reported: Measured<Money>
  actual_billed: Measured<Money>
}

export interface LatencySummary {
  average_ms: Measured<number>
  median_ms: Measured<number>
  p95_ms: Measured<number>
  time_to_first_token_ms: Measured<number>
  output_tokens_per_sec: Measured<number>
}

export interface TaskMetrics {
  task_id: string
  status: TaskStatus
  bands: TokenBands
  total_tokens: Measured<number>
  reasoning_tokens: Measured<number>
  requests: RequestCounts
  elapsed_ms: number
  model_id: string | null
  cost: CostBreakdown
  latency: LatencySummary
}

export type AdapterState = 'ready' | 'detected' | 'not_installed' | 'error'

export type CapabilityState =
  | { state: 'supported'; evidence: string }
  | { state: 'degraded'; evidence: string; caveat: string }
  | { state: 'unsupported'; reason: string }
  | { state: 'unknown'; reason: string }

export interface AdapterDescriptor {
  id: string
  display_name: string
  state: AdapterState
  app_version: string | null
  adapter_version: string
  executable_path: string | null
  capabilities: [string, CapabilityState][]
  notes: string[]
}

export interface IngestProgress {
  files_done: number
  files_total: number
  bytes_done: number
  bytes_total: number
  lag_ms: number
  anomalies: number
}

/** One agent session the monitor has read, whether or not a task claims it. */
export interface SessionSummary {
  session_id: string
  adapter_id: string
  model_id: string | null
  requests: number
  bands: TokenBands
  total_tokens: Measured<number>
  reasoning_tokens: Measured<number>
  first_at: string | null
  last_at: string | null
  /** No task has claimed this session. Shown, never hidden. */
  unattributed: boolean
}

export interface IngestStatus {
  passes: number
  files_scanned: number
  /** Files examined and found unchanged; high is healthy. */
  files_skipped: number
  requests_recorded: number
  anomalies: number
  /** True until the first pass over existing history completes. */
  backfilling: boolean
}

/** One time bucket of usage, for a chart. Bucketed by the backend. */
export interface SeriesPoint {
  at: string
  requests: number
  input_fresh: number
  cache_read: number
  cache_write: number
  output_total: number
  unclassified: number
}

// ── Pricing ─────────────────────────────────────────────────────────────────

/** One model this machine has actually used. */
export interface ObservedModel {
  model_id: string
  adapter_id: string
  requests: number
  total_tokens: number
  /** A rate exists for this exact id. Never true because a similar model has one. */
  priced: boolean
}

/**
 * One version of one model's rates, in money per million tokens.
 *
 * Rates are `Money`, so they arrive as decimal strings for the same reason
 * costs do: parsing one into a float reintroduces the error the string encoding
 * exists to prevent.
 */
export interface PriceRow {
  version_id: string
  model_id: string
  input_per_mtok: Money
  output_per_mtok: Money
  cache_read_per_mtok: Money
  cache_write_5m_per_mtok: Money
  cache_write_1h_per_mtok: Money
  effective_from: string
  /** `seed`, `user` or `updater`. */
  source: string
  /** The version a request made now would be costed with. */
  is_current: boolean
}

/** An exchange rate, and how much to trust it. */
export interface FxRow {
  quote_currency: string
  /** Units of the quote currency per 1 USD. */
  rate: Money
  as_of: string
  source: string
  age_days: number
  /** Past a week old; amounts converted with it are downgraded to estimates. */
  is_stale: boolean
  description: string
}

export interface PricingView {
  models: ObservedModel[]
  prices: PriceRow[]
  fx: FxRow[]
  /** What this backend can present. The picker offers exactly these. */
  supported_currencies: string[]
}

/** A rate the user has entered. */
export interface NewPrice {
  model_id: string
  input_per_mtok: Money
  output_per_mtok: Money
  /**
   * Absent means "charged at the input rate", which is both providers'
   * documented default — not zero, which would claim caching is free.
   */
  cache_read_per_mtok?: Money | null
  cache_write_5m_per_mtok?: Money | null
  cache_write_1h_per_mtok?: Money | null
  note?: string | null
}

export interface NewFxRate {
  quote_currency: string
  rate: Money
}

// ── Comparison ──────────────────────────────────────────────────────────────

/**
 * One task's figures divided by the work it did.
 *
 * There is deliberately no normalized duration. Wall-clock between transcript
 * writes contains tool execution, retry backoff and think time, so "ms per
 * 1,000 output tokens" would look like throughput while mostly measuring how
 * long a file search took.
 */
export interface Normalized {
  basis: string
  /** The divisor, shown so a reader can check the arithmetic. */
  denominator: number
  total_tokens: Measured<number>
  input_tokens: Measured<number>
  cost: Measured<Money>
}

export interface ComparisonRow {
  task_id: string
  name: string
  metrics: TaskMetrics
  /** Null when the task has produced no output to divide by — not zero. */
  normalized: Normalized | null
}

export interface Comparison {
  rows: ComparisonRow[]
  /** Why these rows are not straightforwardly comparable. Shown, not hidden. */
  caveats: string[]
}
