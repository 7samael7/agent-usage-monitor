/**
 * HTTP client for the sidecar.
 *
 * One exported function per route. The bearer token is injected from the
 * connection the host handed us; nothing here knows how it was obtained.
 */

import type { HealthResponse, MetaResponse } from '@aum/api-contract'

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
