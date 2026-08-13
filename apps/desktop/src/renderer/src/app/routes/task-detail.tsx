/**
 * One task in detail: its numbers, its shape over time, and its export.
 */

import type { SeriesPoint, TaskMetrics } from '@aum/api-contract'
import { inputSideTotal } from '@aum/api-contract'
import { useEffect, useState } from 'react'
import { useConnection } from '../../backend/backend-provider'
import { fetchExport, fetchTaskMetrics, fetchTaskSeries } from '../../backend/client'
import { bridge } from '../../platform/bridge'
import { TokensOverTime } from '../../viz/charts'
import { Money, TokenCount } from '../../viz/measurement'
import { Button, Card, Empty, Screen, Stat } from '../../viz/ui'
import { useCurrency } from '../preferences'
import { navigate } from '../router'

export function TaskDetail({ taskId }: { taskId: string }) {
  const conn = useConnection()
  const [metrics, setMetrics] = useState<TaskMetrics | null>(null)
  const [series, setSeries] = useState<SeriesPoint[]>([])
  const [error, setError] = useState<string | null>(null)
  const currency = useCurrency()

  useEffect(() => {
    if (!conn) return
    const ac = new AbortController()

    const load = () => {
      fetchTaskMetrics(conn, taskId, currency, ac.signal)
        .then(setMetrics)
        .catch((e: unknown) => {
          if (!ac.signal.aborted) setError(String(e))
        })
      fetchTaskSeries(conn, taskId, 60, ac.signal)
        .then(setSeries)
        .catch(() => {})
    }

    load()
    const timer = setInterval(load, 2_000)
    return () => {
      ac.abort()
      clearInterval(timer)
    }
  }, [conn, taskId, currency])

  const save = (format: 'json' | 'csv') => {
    if (!conn) return
    void (async () => {
      const [text, path] = await Promise.all([
        fetchExport(conn, taskId, format, currency),
        bridge.native.pickSavePath({
          defaultName: `task-${taskId.slice(0, 8)}.${format}`,
          filters: [{ name: format.toUpperCase(), extensions: [format] }],
        }),
      ])
      if (!path) return
      // Written by the host, which has filesystem access; the interface does
      // not, and should not acquire it just to save a file.
      await bridge.native.writeTextFile(path, text)
    })()
  }

  if (error) {
    return (
      <Screen title="Task">
        <p className="text-neg">{error}</p>
      </Screen>
    )
  }

  if (!metrics) {
    return (
      <Screen title="Task">
        <Empty>Loading…</Empty>
      </Screen>
    )
  }

  return (
    <Screen
      title="Task detail"
      subtitle={`${metrics.model_id ?? 'model unknown'} · ${metrics.status}`}
      actions={
        <>
          <Button onClick={() => navigate('/live')}>Back</Button>
          <Button onClick={() => save('json')}>Export JSON</Button>
          <Button onClick={() => save('csv')}>Export CSV</Button>
        </>
      }
    >
      <section className="mb-5 grid max-w-4xl grid-cols-4 gap-3">
        <Stat label="total tokens" value={<TokenCount measured={metrics.total_tokens} />} />
        <Stat
          label="input"
          value={inputSideTotal(metrics.bands).toLocaleString('en-US')}
          hint="Fresh input plus cache reads plus cache writes — the only input quantity that means the same thing for every agent."
        />
        <Stat label="output" value={metrics.bands.output_total.toLocaleString('en-US')} />
        <Stat
          label="reasoning"
          value={<TokenCount measured={metrics.reasoning_tokens} showTag={false} />}
        />
      </section>

      <section className="mb-5 grid max-w-4xl grid-cols-3 gap-3">
        <Stat
          label="API-equivalent"
          value={
            <Money
              measured={metrics.cost.api_equivalent}
              kind="api-equivalent"
              currency={currency}
            />
          }
          hint="What this would cost on pay-as-you-go. Not a charge."
        />
        <Stat
          label="actually billed"
          value={<Money measured={metrics.cost.actual_billed} kind="billed" currency={currency} />}
          hint="What you were really charged."
        />
        <Stat
          label="requests"
          value={`${metrics.requests.is_lower_bound ? '≥' : ''}${metrics.requests.succeeded}${
            metrics.requests.failed > 0 ? ` (+${metrics.requests.failed} failed)` : ''
          }`}
        />
      </section>

      <Card title="Tokens over time" className="max-w-4xl">
        <TokensOverTime series={series} />
        <p className="mt-2 text-[10px] text-text-mute leading-relaxed">
          Bands are stacked because they do not overlap. A break in the line is a period with no
          measurement, not a period of zero usage.
        </p>
      </Card>
    </Screen>
  )
}
