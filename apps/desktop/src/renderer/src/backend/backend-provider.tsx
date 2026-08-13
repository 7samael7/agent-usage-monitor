/**
 * Backend connection state.
 *
 * The renderer holds `{ baseUrl, token, generation }`. When `generation`
 * changes the backend restarted, and every piece of live state must be
 * discarded rather than replayed into a new backend generation — reusing a
 * pre-restart store against a new one is a silent-corruption bug, not a cosmetic
 * one.
 */

import { createContext, useContext, useEffect, useMemo, useState } from 'react'
import type { ReactNode } from 'react'
import type { BackendInfo } from '../platform/bridge'
import { bridge } from '../platform/bridge'

interface BackendContextValue {
  readonly info: BackendInfo
  readonly restart: () => void
}

const BackendContext = createContext<BackendContextValue | null>(null)

const UNKNOWN: BackendInfo = {
  phase: 'idle',
  generation: 0,
  detail: null,
  baseUrl: null,
  token: null,
  contractVersion: null,
}

export function BackendProvider({ children }: { children: ReactNode }) {
  const [info, setInfo] = useState<BackendInfo>(UNKNOWN)

  useEffect(() => {
    let cancelled = false
    void bridge.backend.info().then((i) => {
      if (!cancelled) setInfo(i)
    })
    const off = bridge.backend.onStateChange(setInfo)
    return () => {
      cancelled = true
      off()
    }
  }, [])

  const value = useMemo<BackendContextValue>(
    () => ({
      info,
      restart: () => {
        void bridge.backend.restart().then(setInfo)
      },
    }),
    [info],
  )

  return <BackendContext.Provider value={value}>{children}</BackendContext.Provider>
}

export function useBackend(): BackendContextValue {
  const ctx = useContext(BackendContext)
  if (!ctx) throw new Error('useBackend must be used inside <BackendProvider>')
  return ctx
}

/**
 * The connection, or `null` when the backend is not usable.
 *
 * Components that need to fetch should render nothing rather than fetching
 * against a stale base URL.
 */
export function useConnection(): { baseUrl: string; token: string; generation: number } | null {
  const { info } = useBackend()
  return useMemo(() => {
    if (!info.baseUrl || !info.token) return null
    if (info.phase !== 'ready' && info.phase !== 'degraded') return null
    return { baseUrl: info.baseUrl, token: info.token, generation: info.generation }
  }, [info])
}
