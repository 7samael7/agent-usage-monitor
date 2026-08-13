/**
 * HTTP client for the sidecar.
 *
 * One exported function per route. The bearer token is injected from the
 * connection the host handed us; nothing here knows how it was obtained.
 */

import type {
  AdapterDescriptor,
  Comparison,
  FxRow,
  HealthResponse,
  IngestStatus,
  MetaResponse,
  NewFxRate,
  NewPrice,
  PriceRow,
  PricingView,
  SeriesPoint,
  SessionSummary,
  TaskMetrics,
  TaskSummary,
} from '@aum/api-contract'

export interface Connection {
  readonly baseUrl: string
  readonly token: string
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly detail: string,
  ) {
    super(`HTTP ${status}: ${detail}`)
    this.name = 'ApiError'
  }
}

async function get<T>(conn: Connection, path: string, signal?: AbortSignal): Promise<T> {
  const res = await fetch(`${conn.baseUrl}${path}`, {
    headers: { Authorization: `Bearer ${conn.token}` },
    signal: signal ?? null,
  })
  if (!res.ok) {
    throw new ApiError(res.status, await res.text().catch(() => res.statusText))
  }
  return (await res.json()) as T
}

export function fetchHealth(conn: Connection, signal?: AbortSignal): Promise<HealthResponse> {
  return get(conn, '/v1/health', signal)
}

export function fetchMeta(conn: Connection, signal?: AbortSignal): Promise<MetaResponse> {
  return get(conn, '/v1/meta', signal)
}

export function fetchIngestStatus(conn: Connection, signal?: AbortSignal): Promise<IngestStatus> {
  return get(conn, '/v1/ingest/status', signal)
}

export function fetchSessions(conn: Connection, signal?: AbortSignal): Promise<SessionSummary[]> {
  return get(conn, '/v1/sessions', signal)
}

export function fetchTasks(conn: Connection, signal?: AbortSignal): Promise<TaskSummary[]> {
  return get(conn, '/v1/tasks', signal)
}

export function fetchTaskMetrics(
  conn: Connection,
  taskId: string,
  currency?: string,
  signal?: AbortSignal,
): Promise<TaskMetrics> {
  // The backend does the conversion, in decimal, and marks what it cost in
  // certainty. Converting here would mean float arithmetic on money and would
  // lose the accuracy downgrade along with it.
  const q = currency && currency !== 'USD' ? `?currency=${currency}` : ''
  return get(conn, `/v1/tasks/${taskId}/metrics${q}`, signal)
}

export function fetchAdapters(
  conn: Connection,
  signal?: AbortSignal,
): Promise<AdapterDescriptor[]> {
  return get(conn, '/v1/adapters', signal)
}

export interface NewTask {
  name: string
  adapter_id: string
  working_dir: string
  prompt: string
  /**
   * Environment for the agent process.
   *
   * Sent, used, and never stored or echoed back — these routinely hold API
   * keys, and the monitor has no business keeping them.
   */
  env?: [string, string][]
}

export async function createTask(conn: Connection, task: NewTask): Promise<TaskSummary> {
  const res = await fetch(`${conn.baseUrl}/v1/tasks`, {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${conn.token}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify(task),
  })
  if (!res.ok) {
    throw new ApiError(res.status, await res.text().catch(() => res.statusText))
  }
  return (await res.json()) as TaskSummary
}

export async function stopTask(conn: Connection, taskId: string): Promise<void> {
  const res = await fetch(`${conn.baseUrl}/v1/tasks/${taskId}/stop`, {
    method: 'POST',
    headers: { Authorization: `Bearer ${conn.token}` },
  })
  if (!res.ok) {
    throw new ApiError(res.status, await res.text().catch(() => res.statusText))
  }
}

export function fetchTaskSeries(
  conn: Connection,
  taskId: string,
  bucketSeconds = 60,
  signal?: AbortSignal,
): Promise<SeriesPoint[]> {
  return get(conn, `/v1/tasks/${taskId}/series?bucket_seconds=${bucketSeconds}`, signal)
}

/** A URL the user can open or save. The export itself is metadata only. */
export function exportUrl(
  conn: Connection,
  taskId: string,
  format: 'json' | 'csv',
  currency?: string,
): string {
  const cur = currency && currency !== 'USD' ? `&currency=${currency}` : ''
  return `${conn.baseUrl}/v1/tasks/${taskId}/export?format=${format}${cur}`
}

/**
 * Fetch an export as text.
 *
 * Goes through fetch rather than a plain link because the API needs a bearer
 * header, and a link cannot carry one — putting the token in a query string
 * would leak it into logs and history.
 */
export async function fetchExport(
  conn: Connection,
  taskId: string,
  format: 'json' | 'csv',
  currency?: string,
): Promise<string> {
  const res = await fetch(exportUrl(conn, taskId, format, currency), {
    headers: { Authorization: `Bearer ${conn.token}` },
  })
  if (!res.ok) throw new ApiError(res.status, await res.text().catch(() => res.statusText))
  return res.text()
}

async function post<T>(conn: Connection, path: string, body: unknown): Promise<T> {
  const res = await fetch(`${conn.baseUrl}${path}`, {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${conn.token}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify(body),
  })
  if (!res.ok) throw new ApiError(res.status, await res.text().catch(() => res.statusText))
  return (await res.json()) as T
}

export function fetchPricing(conn: Connection, signal?: AbortSignal): Promise<PricingView> {
  return get(conn, '/v1/pricing', signal)
}

export function savePrice(conn: Connection, price: NewPrice): Promise<PriceRow> {
  return post(conn, '/v1/pricing', price)
}

export function saveFxRate(conn: Connection, rate: NewFxRate): Promise<FxRow> {
  return post(conn, '/v1/pricing/fx', rate)
}

export function fetchComparison(
  conn: Connection,
  taskIds: string[],
  options: { currency?: string; normalize?: boolean } = {},
  signal?: AbortSignal,
): Promise<Comparison> {
  const params = new URLSearchParams({ tasks: taskIds.join(',') })
  if (options.currency && options.currency !== 'USD') params.set('currency', options.currency)
  if (options.normalize) params.set('normalize', 'true')
  return get(conn, `/v1/compare?${params.toString()}`, signal)
}
