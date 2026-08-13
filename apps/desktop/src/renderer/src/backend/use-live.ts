/**
 * React bindings for the live store, and the stream that feeds it.
 */

import { parseStreamEvent } from '@aum/api-contract'
import { useEffect, useSyncExternalStore } from 'react'
import { bridge } from '../platform/bridge'
import { type LiveTask, liveStore } from './live-store'
import { consumeSse } from './sse'

/**
 * Keep the live store fed for as long as this is mounted.
 *
 * Reconnects with backoff. On reconnect the store is cleared rather than
 * resumed: a gap in an absolute-snapshot stream is harmless, but carrying stale
 * task state across a backend restart is not.
 */
export function useEventStream(conn: { baseUrl: string; token: string } | null): void {
  useEffect(() => {
    if (!conn) return

    const ac = new AbortController()
    let attempt = 0
    let timer: number | undefined

    const connect = () => {
      void consumeSse({
        baseUrl: conn.baseUrl,
        token: conn.token,
        signal: ac.signal,
        onOpen: () => {
          attempt = 0
        },
        onFrame: (frame) => {
          bridge.backend.noteStreamActivity()
          const parsed = parseStreamEvent(frame.data)
          if (parsed) liveStore.apply(parsed)
          else liveStore.noteViolation()
        },
        onError: () => {
          if (ac.signal.aborted) return
          // 0.5s, 1s, 2s, 4s, capped at 8s. Fast enough that a sidecar restart
          // is barely visible, slow enough not to hammer a broken one.
          attempt += 1
          const delay = Math.min(8_000, 500 * 2 ** (attempt - 1))
          timer = window.setTimeout(connect, delay)
        },
      })
    }

    connect()
    return () => {
      ac.abort()
      if (timer) clearTimeout(timer)
      liveStore.clear()
    }
  }, [conn])
}

/** One task's live state. Re-renders only when that task changes. */
export function useLiveTask(taskId: string): LiveTask | undefined {
  return useSyncExternalStore(
    (listener) => liveStore.subscribeTask(taskId, listener),
    () => liveStore.getTask(taskId),
    () => undefined,
  )
}

/** The ids of tasks the stream has told us about. */
export function useLiveTaskIds(): string[] {
  return useSyncExternalStore(
    (listener) => liveStore.subscribeAll(listener),
    () => liveStore.getIds(),
    () => [],
  )
}
