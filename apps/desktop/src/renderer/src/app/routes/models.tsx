/**
 * Models and pricing.
 *
 * Prices are append-only: correcting one adds a version rather than editing a
 * row, so a benchmark run in March still shows March's numbers. A model with no
 * price is shown as such rather than being given a plausible-looking one.
 */

import type { SessionSummary } from '@aum/api-contract'
import { useEffect, useState } from 'react'
import { useConnection } from '../../backend/backend-provider'
import { fetchSessions } from '../../backend/client'
import { Empty, Row, Screen, Table, Td, Th } from '../../viz/ui'

export function Models() {
  const conn = useConnection()
  const [sessions, setSessions] = useState<SessionSummary[] | null>(null)

  useEffect(() => {
    if (!conn) return
    const ac = new AbortController()
    fetchSessions(conn, ac.signal)
      .then(setSessions)
      .catch(() => {})
    return () => ac.abort()
  }, [conn])

  // Models this machine has actually used, which is the list that matters —
  // not a catalogue of everything a provider publishes.
  const observed = new Map<string, { adapter: string; requests: number }>()
  for (const s of sessions ?? []) {
    if (!s.model_id) continue
    const existing = observed.get(s.model_id)
    observed.set(s.model_id, {
      adapter: s.adapter_id,
      requests: (existing?.requests ?? 0) + s.requests,
    })
  }

  return (
    <Screen
      title="Models & Pricing"
      subtitle="Models seen on this machine. A model with no published price is shown as unpriced rather than being costed at a similar model's rate."
    >
      {!sessions && <Empty>Loading…</Empty>}

      {observed.size > 0 && (
        <Table
          head={
            <>
              <Th>model</Th>
              <Th>agent</Th>
              <Th align="right">requests seen</Th>
              <Th>pricing</Th>
            </>
          }
        >
          {[...observed.entries()]
            .sort((a, b) => b[1].requests - a[1].requests)
            .map(([model, info]) => (
              <Row key={model}>
                <Td>{model}</Td>
                <Td>{info.adapter}</Td>
                <Td align="right" numeric>
                  {info.requests.toLocaleString('en-US')}
                </Td>
                <Td>
                  <span className="text-warn" title="No published rate exists for this model yet.">
                    not priced
                  </span>
                </Td>
              </Row>
            ))}
        </Table>
      )}

      <p className="mt-4 max-w-3xl text-[11px] text-text-mute leading-relaxed">
        Every model this machine runs is newer than any published price list, so costs currently
        show as unavailable. That is deliberate: pricing one of them at a similar model's rate would
        produce a confident total wrong by an unknown factor. Editing prices from this screen is not
        implemented yet; when it is, an edit will add a new version rather than change an existing
        one, so past runs keep the numbers they were computed with.
      </p>
    </Screen>
  )
}
