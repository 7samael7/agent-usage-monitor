/**
 * The dashboard.
 *
 * Shows what has actually been read from the agents' own records. There is
 * still no benchmark or live-task machinery here, and deliberately no
 * placeholder standing in for one: an invented number is precisely the failure
 * this application exists to avoid.
 */

import type { IngestStatus, SessionSummary } from '@aum/api-contract'
import { inputSideTotal } from '@aum/api-contract'
import { useEffect, useState } from 'react'
import { useConnection } from '../backend/backend-provider'
import { fetchIngestStatus, fetchSessions } from '../backend/client'
import { TokenCount } from '../viz/measurement'

export function Dashboard() {
  const conn = useConnection()
  const [status, setStatus] = useState<IngestStatus | null>(null)
  const [sessions, setSessions] = useState<SessionSummary[] | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (!conn) return
    const ac = new AbortController()

    const load = () => {
      fetchIngestStatus(conn, ac.signal)
        .then(setStatus)
        .catch((e: unknown) => {
          if (!ac.signal.aborted) setError(String(e))
        })
      fetchSessions(conn, ac.signal)
        .then(setSessions)
        .catch(() => {
          /* the status request already surfaces a failure */
        })
    }

    load()
    // Modest interval: this is history, not a live stream. Live task numbers
    // will arrive over SSE rather than by polling harder.
    const timer = setInterval(load, 2_000)
    return () => {
      ac.abort()
      clearInterval(timer)
    }
  }, [conn])

  return (
    <div className="p-6">
      <h1 className="mb-1 font-semibold text-[15px]">Dashboard</h1>
      <p className="mb-6 text-text-mute">
        Usage read from the agents' own records on this machine.
      </p>

      {error && <p className="mb-4 text-neg">{error}</p>}

      <IngestSummary status={status} />
      <SessionTable sessions={sessions} status={status} />
    </div>
  )
}

function IngestSummary({ status }: { status: IngestStatus | null }) {
  if (!status) return <p className="text-text-mute">Connecting…</p>

  return (
    <section className="mb-6 grid max-w-3xl grid-cols-4 gap-3">
      <Stat label="requests read" value={status.requests_recorded.toLocaleString('en-US')} />
      <Stat label="file scans" value={status.files_scanned.toLocaleString('en-US')} />
      <Stat label="passes" value={String(status.passes)} />
      <Stat
        label="anomalies"
        value={String(status.anomalies)}
        tone={status.anomalies > 0 ? 'warn' : undefined}
      />
    </section>
  )
}

function Stat({
  label,
  value,
  tone,
}: {
  label: string
  value: string
  tone?: 'warn'
}) {
  return (
    <div className="rounded-md border border-border bg-surface px-3 py-2">
      <div className="mb-0.5 text-[10px] text-text-mute uppercase tracking-wide">{label}</div>
      <div className={`numeric text-[16px] ${tone === 'warn' ? 'text-warn' : 'text-text'}`}>
        {value}
      </div>
    </div>
  )
}

function SessionTable({
  sessions,
  status,
}: {
  sessions: SessionSummary[] | null
  status: IngestStatus | null
}) {
  if (!sessions) return null

  if (sessions.length === 0) {
    return (
      <p className="text-text-mute">
        {/* "Still reading" and "nothing to read" are different statements, and
            conflating them would be its own small dishonesty. */}
        {status?.backfilling
          ? 'Reading your history…'
          : 'No agent usage found. Claude Code and Codex record usage as they run; once either has, it will appear here.'}
      </p>
    )
  }

  return (
    <section>
      <h2 className="mb-2 font-medium text-[11px] text-text-mute uppercase tracking-wide">
        Recent sessions
      </h2>
      <div className="overflow-x-auto rounded-md border border-border">
        <table className="w-full border-collapse text-left">
          <thead>
            <tr className="border-border border-b bg-surface-2 text-[10px] text-text-mute uppercase tracking-wide">
              <Th>agent</Th>
              <Th>model</Th>
              <Th align="right">requests</Th>
              <Th align="right">input</Th>
              <Th align="right">output</Th>
              <Th align="right">reasoning</Th>
              <Th align="right">total</Th>
              <Th>attribution</Th>
            </tr>
          </thead>
          <tbody>
            {sessions.map((s) => (
              <tr key={s.session_id} className="border-border/60 border-b last:border-0">
                <Td>{s.adapter_id}</Td>
                <Td>
                  {/* A session whose model was never declared genuinely has
                      none — a tailer resuming mid-file cannot know it. */}
                  {s.model_id ?? <span className="text-text-mute">unknown</span>}
                </Td>
                <Td align="right" numeric>
                  {s.requests.toLocaleString('en-US')}
                </Td>
                <Td align="right" numeric>
                  {inputSideTotal(s.bands).toLocaleString('en-US')}
                </Td>
                <Td align="right" numeric>
                  {s.bands.output_total.toLocaleString('en-US')}
                </Td>
                <Td align="right">
                  <TokenCount measured={s.reasoning_tokens} showTag={false} />
                </Td>
                <Td align="right">
                  <TokenCount measured={s.total_tokens} showTag={false} />
                </Td>
                <Td>
                  {s.unattributed ? (
                    <span
                      className="text-text-mute"
                      title="No task has claimed this session. Usage is never guessed into a task."
                    >
                      unattributed
                    </span>
                  ) : (
                    <span className="text-exact">task</span>
                  )}
                </Td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      <p className="mt-3 max-w-3xl text-[11px] text-text-mute leading-relaxed">
        Input is fresh input plus cache reads plus cache writes — the only quantity that means the
        same thing for both agents. Claude Code does not report reasoning tokens, so those read as
        unavailable rather than zero.
      </p>
    </section>
  )
}

function Th({
  children,
  align,
}: {
  children: React.ReactNode
  align?: 'right'
}) {
  return (
    <th className={`px-3 py-1.5 font-medium ${align === 'right' ? 'text-right' : ''}`}>
      {children}
    </th>
  )
}

function Td({
  children,
  align,
  numeric,
}: {
  children: React.ReactNode
  align?: 'right'
  numeric?: boolean
}) {
  return (
    <td
      className={`px-3 py-1.5 ${align === 'right' ? 'text-right' : ''} ${numeric ? 'numeric' : ''}`}
    >
      {children}
    </td>
  )
}
