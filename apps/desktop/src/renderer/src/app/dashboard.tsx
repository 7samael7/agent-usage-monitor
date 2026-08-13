/**
 * The dashboard, in its milestone-zero form.
 *
 * There is deliberately nothing here that resembles usage data yet. The
 * adapters that produce real numbers do not exist, and a placeholder card
 * showing plausible-looking tokens would be exactly the kind of thing this
 * project exists not to do. What it does show is the connection itself — proof
 * that the handshake, authentication and the event stream all work end to end.
 */

import type { MetaResponse } from '@aum/api-contract'
import { parseStreamEvent } from '@aum/api-contract'
import { useEffect, useState } from 'react'
import { useConnection } from '../backend/backend-provider'
import { fetchMeta } from '../backend/client'
import { consumeSse } from '../backend/sse'
import { bridge } from '../platform/bridge'

interface StreamLine {
  seq: number
  event: string
  at: string
}

export function Dashboard() {
  const conn = useConnection()
  const [meta, setMeta] = useState<MetaResponse | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [streamOpen, setStreamOpen] = useState(false)
  const [lines, setLines] = useState<StreamLine[]>([])
  const [violations, setViolations] = useState(0)

  useEffect(() => {
    if (!conn) return
    const ac = new AbortController()
    setMeta(null)
    setError(null)
    setLines([])
    setViolations(0)

    fetchMeta(conn, ac.signal)
      .then(setMeta)
      .catch((e: unknown) => {
        if (!ac.signal.aborted) setError(String(e))
      })

    void consumeSse({
      baseUrl: conn.baseUrl,
      token: conn.token,
      signal: ac.signal,
      onOpen: () => setStreamOpen(true),
      onFrame: (frame) => {
        bridge.backend.noteStreamActivity()
        const parsed = parseStreamEvent(frame.data)
        if (!parsed) {
          setViolations((v) => v + 1)
          return
        }
        setLines((prev) =>
          [{ seq: parsed.seq, event: frame.event, at: parsed.ts }, ...prev].slice(0, 20),
        )
      },
      onError: () => setStreamOpen(false),
    })

    return () => {
      ac.abort()
      setStreamOpen(false)
    }
    // `conn` is memoized and carries `generation`, so a backend restart yields a
    // new object and re-runs this effect — which is exactly what must happen:
    // live state from a previous backend generation must be discarded, never
    // resumed against a new process.
  }, [conn])

  return (
    <div className="p-6">
      <h1 className="mb-1 font-semibold text-[15px]">Dashboard</h1>
      <p className="mb-6 text-text-mute">
        No adapters are collecting yet. Usage will appear here once the Claude Code and Codex
        adapters land.
      </p>

      <section className="grid max-w-4xl grid-cols-2 gap-3">
        <Card title="Backend">
          {error ? (
            <p className="text-neg">{error}</p>
          ) : meta ? (
            <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1">
              <Row label="implementation" value={`${meta.impl_name} ${meta.impl_version}`} />
              <Row label="contract" value={meta.contract_version} />
              <Row label="stream epoch" value={meta.stream_epoch.slice(0, 8)} />
              <Row label="capabilities" value={meta.capabilities.join(', ') || '—'} />
            </dl>
          ) : (
            <p className="text-text-mute">Loading…</p>
          )}
        </Card>

        <Card title="Event stream">
          <p className="mb-2 flex items-center gap-1.5 text-text-dim">
            <span
              className={`size-1.5 rounded-full ${streamOpen ? 'bg-exact' : 'bg-na'}`}
              aria-hidden
            />
            {streamOpen ? 'connected' : 'not connected'}
          </p>
          {violations > 0 && (
            <p className="mb-2 text-warn">
              {violations} frame{violations === 1 ? '' : 's'} could not be understood by this build.
            </p>
          )}
          {lines.length === 0 ? (
            <p className="text-text-mute">
              Waiting for events. A heartbeat arrives every 10 seconds.
            </p>
          ) : (
            <ul className="numeric flex flex-col gap-0.5 font-mono text-[11px] text-text-mute">
              {lines.map((l) => (
                <li key={l.seq}>
                  <span className="text-text-faint">#{l.seq}</span> {l.event}
                </li>
              ))}
            </ul>
          )}
        </Card>
      </section>
    </div>
  )
}

function Card({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="rounded-md border border-border bg-surface p-4">
      <h2 className="mb-3 font-medium text-[11px] text-text-mute uppercase tracking-wide">
        {title}
      </h2>
      {children}
    </div>
  )
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <>
      <dt className="text-text-mute">{label}</dt>
      <dd className="numeric font-mono text-[12px] text-text-dim">{value}</dd>
    </>
  )
}
