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
