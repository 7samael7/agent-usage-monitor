/**
 * Everything the monitor has read, including sessions no task claimed.
 */

import type { SessionSummary } from '@aum/api-contract'
import { inputSideTotal } from '@aum/api-contract'
import { useEffect, useState } from 'react'
import { useConnection } from '../../backend/backend-provider'
import { fetchSessions } from '../../backend/client'
import { TokenCount } from '../../viz/measurement'
import { Empty, Row, Screen, Table, Td, Th } from '../../viz/ui'

export function History() {
  const conn = useConnection()
  const [sessions, setSessions] = useState<SessionSummary[] | null>(null)
  const [agent, setAgent] = useState<string>('all')

  useEffect(() => {
    if (!conn) return
    const ac = new AbortController()
    fetchSessions(conn, ac.signal)
      .then(setSessions)
      .catch(() => {})
    return () => ac.abort()
  }, [conn])

  const filtered = (sessions ?? []).filter((s) => agent === 'all' || s.adapter_id === agent)

  return (
    <Screen
      title="History"
      subtitle="Sessions read from the agents' own records, whether or not a task claimed them."
      actions={
        <select
          className="rounded border border-border bg-surface-inset px-2 py-1.5 text-text"
          value={agent}
          onChange={(e) => setAgent(e.target.value)}
        >
          <option value="all">All agents</option>
          <option value="claude_code">Claude Code</option>
          <option value="codex">Codex</option>
        </select>
      }
    >
      {!sessions && <Empty>Loading…</Empty>}
      {sessions && filtered.length === 0 && <Empty>Nothing recorded for this filter.</Empty>}

      {filtered.length > 0 && (
        <Table
          head={
            <>
              <Th>agent</Th>
              <Th>model</Th>
              <Th>last activity</Th>
              <Th align="right">requests</Th>
              <Th align="right">input</Th>
              <Th align="right">output</Th>
              <Th align="right">reasoning</Th>
              <Th align="right">total</Th>
              <Th>attribution</Th>
            </>
          }
        >
          {filtered.map((s) => (
            <Row key={s.session_id}>
              <Td>{s.adapter_id}</Td>
              <Td>{s.model_id ?? <span className="text-text-mute">unknown</span>}</Td>
              <Td title={s.last_at ?? ''}>{s.last_at?.slice(0, 16).replace('T', ' ') ?? '—'}</Td>
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
                    title="No task claimed this session. Usage is never guessed into one."
                  >
                    unattributed
                  </span>
                ) : (
                  <span className="text-exact">task</span>
                )}
              </Td>
            </Row>
          ))}
        </Table>
      )}
    </Screen>
  )
}
