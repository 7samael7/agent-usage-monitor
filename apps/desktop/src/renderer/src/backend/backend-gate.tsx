/**
 * What the user sees when the backend is not ready.
 *
 * A full-window gate rather than a toast, because the app is inert without a
 * backend — except while *restarting*, where the UI stays mounted and live
 * numbers are marked stale instead. Losing the whole screen because of a
 * two-second reconnect would be worse than the reconnect.
 */

import { useEffect, useState } from 'react'
import type { ReactNode } from 'react'
import { bridge } from '../platform/bridge'
import { useBackend } from './backend-provider'

export function BackendGate({ children }: { children: ReactNode }) {
  const { info, restart } = useBackend()
  const [logs, setLogs] = useState<string[]>([])
  const [slow, setSlow] = useState(false)

  const isStarting = info.phase === 'spawning' || info.phase === 'handshaking'
  const isDown = info.phase === 'failed' || info.phase === 'version-mismatch'

  useEffect(() => {
    if (!isDown) return
    void bridge.backend.logs().then(setLogs)
  }, [isDown])

  useEffect(() => {
    if (!isStarting) {
      setSlow(false)
      return
    }
    const t = setTimeout(() => setSlow(true), 3_000)
    return () => clearTimeout(t)
  }, [isStarting])

  if (isStarting || info.phase === 'idle') {
    return (
      <Centered>
        <div className="flex flex-col items-center gap-3">
          <Spinner />
          <p className="text-text-dim">Starting the monitoring backend…</p>
          {slow && <p className="text-text-mute text-xs">This is taking longer than usual.</p>}
        </div>
      </Centered>
    )
  }

  if (isDown) {
    const isMismatch = info.phase === 'version-mismatch'
    return (
      <Centered>
        <div className="w-full max-w-2xl">
          <h1 className="mb-1 font-semibold text-[15px] text-neg">
            {isMismatch ? 'Backend version mismatch' : 'The monitoring backend did not start'}
          </h1>
          <p className="mb-4 whitespace-pre-wrap text-text-dim leading-relaxed">
            {info.detail ?? 'No further detail is available.'}
          </p>

          {logs.length > 0 && (
            <pre className="mb-4 max-h-64 overflow-auto rounded border border-border bg-surface-inset p-3 font-mono text-[11px] text-text-mute leading-relaxed">
              {logs.slice(-40).join('\n')}
            </pre>
          )}

          <div className="flex gap-2">
            {/* A version mismatch is deliberately not retryable: retrying the
                same two binaries cannot succeed, and offering the button would
                imply otherwise. */}
            {!isMismatch && (
              <button type="button" onClick={restart} className={buttonClass}>
                Try again
              </button>
            )}
            <button
              type="button"
              onClick={() => void navigator.clipboard.writeText(diagnostics(info, logs))}
              className={buttonClass}
            >
              Copy diagnostics
            </button>
          </div>
        </div>
      </Centered>
    )
  }

  // ready · degraded · restarting · stopping — keep the app mounted. Banners for
  // these live in the shell so the user does not lose their place.
  return <>{children}</>
}

function diagnostics(info: { phase: string; detail: string | null }, logs: string[]): string {
  return [
    `phase: ${info.phase}`,
    `detail: ${info.detail ?? '(none)'}`,
    '',
    '--- backend stderr (last 40 lines) ---',
    ...logs.slice(-40),
  ].join('\n')
}

const buttonClass =
  'no-drag rounded border border-border-strong bg-surface-2 px-3 py-1.5 text-text ' +
  'transition-colors hover:bg-surface-3 active:bg-surface'

function Centered({ children }: { children: ReactNode }) {
  return (
    <div className="drag-region flex h-full w-full items-center justify-center p-8">
      <div className="no-drag">{children}</div>
    </div>
  )
}

function Spinner() {
  return (
    <div
      className="size-5 animate-spin rounded-full border-2 border-border-strong border-t-accent"
      role="status"
      aria-label="Loading"
    />
  )
}
