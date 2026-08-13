/**
 * HTTP client for the sidecar.
 *
 * One exported function per route. The bearer token is injected from the
 * connection the host handed us; nothing here knows how it was obtained.
 */

import type {
  AdapterDescriptor,
  HealthResponse,
  IngestStatus,
  MetaResponse,
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
  signal?: AbortSignal,
): Promise<TaskMetrics> {
  return get(conn, `/v1/tasks/${taskId}/metrics`, signal)
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
