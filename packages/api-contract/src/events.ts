/** Mirrors `aum_contract::events`. */

import type { AdapterState, IngestProgress, TaskMetrics, TaskSummary } from './dto'
import type { MeasurementSource } from './measurement'
import type { TokenBands } from './tokens'

export type AgentEvent =
  | { type: 'heartbeat'; lag_ms: number }
  | { type: 'task_created'; task: TaskSummary }
  | { type: 'task_updated'; task: TaskSummary }
  | { type: 'task_stopped'; exit_code: number | null; reason: string }
  | { type: 'metrics_snapshot'; metrics: TaskMetrics }
  | {
      type: 'usage_delta'
      request_id: string
      model_id: string
      bands: TokenBands
      measurement_source: MeasurementSource
    }
  | { type: 'adapter_state_changed'; adapter_id: string; state: AdapterState }
  | { type: 'ingest_progressed'; progress: IngestProgress }
  | { type: 'attribution_unresolved'; session_id: string; adapter_id: string; reason: string }
  | { type: 'ingest_anomaly'; kind: string; detail: string }
  | { type: 'pricing_updated'; model_id: string }
  | { type: 'resync'; reason: string }

export interface EventEnvelope {
  seq: number
  /** Changes on backend restart: discard live state rather than replaying it. */
  stream_epoch: string
  ts: string
  task_id?: string | null
}

export type StreamEvent = EventEnvelope & AgentEvent

/**
 * Parse and validate one frame's payload.
 *
 * Returns `null` for anything unrecognised rather than throwing. A malformed
 * event must not take down the render tree — but it must not be silently
 * applied either, so the caller counts violations and warns the user when the
 * backend is sending data this build does not understand.
 */
export function parseStreamEvent(raw: string): StreamEvent | null {
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    return null
  }
  if (typeof parsed !== 'object' || parsed === null) return null

  const candidate = parsed as Record<string, unknown>
  if (typeof candidate.type !== 'string') return null
  if (typeof candidate.seq !== 'number') return null
  if (typeof candidate.stream_epoch !== 'string') return null

  return candidate as unknown as StreamEvent
}
