/**
 * Live Tasks.
 *
 * Numbers arrive over the event stream and are read straight from the live
 * store, so a burst of updates re-renders the cells that changed rather than
 * the whole table.
 */

import type { TaskSummary } from '@aum/api-contract'
import { inputSideTotal } from '@aum/api-contract'
import { useEffect, useState } from 'react'
import { useConnection } from '../../backend/backend-provider'
import { fetchTasks, stopTask } from '../../backend/client'
import { useLiveTask } from '../../backend/use-live'
import { Money, TokenCount } from '../../viz/measurement'
import { Button, Empty, Row, Screen, StatusDot, Table, Td, Th } from '../../viz/ui'
import { navigate } from '../router'

export function LiveTasks() {
  const conn = useConnection()
  const [visible, setVisible] = useState<TaskSummary[]>([])
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (!conn) return
    const ac = new AbortController()

    const load = () =>
      fetchTasks(conn, ac.signal)
        .then((tasks) => {
          const live = tasks.filter((t) => t.status === 'running' || t.status === 'pending')
          // A task that fails on launch used to vanish from this screen the
          // instant it failed, which is the worst possible moment to hide it:
          // the user pressed start and the row disappeared. The most recent
          // failures stay, carrying the agent's own explanation.
          const failed = tasks.filter((t) => t.status === 'failed').slice(0, 5)
          setVisible([...live, ...failed])
        })
        .catch((e: unknown) => {
          if (!ac.signal.aborted) setError(String(e))
        })

    load()
    // The task *list* changes when one starts or stops, which is rare. Its
    // numbers come over the stream, so this does not need to be fast.
    const timer = setInterval(load, 3_000)
    return () => {
      ac.abort()
      clearInterval(timer)
    }
  }, [conn])

  return (
    <Screen
      title="Live Tasks"
      subtitle="Tasks the monitor launched and is watching. Numbers update as the agents work."
      actions={
        <Button variant="primary" onClick={() => navigate('/benchmarks')}>
          New task
        </Button>
      }
    >
      {error && <p className="mb-4 text-neg">{error}</p>}

      {visible.length === 0 ? (
        <Empty>
          Nothing is running. Start a task from Benchmarks, and its usage will appear here as the
          agent works.
        </Empty>
      ) : (
        <Table
          head={
            <>
              <Th>task</Th>
              <Th>agent</Th>
              <Th>model</Th>
              <Th align="right">requests</Th>
              <Th align="right">input</Th>
              <Th align="right">output</Th>
              <Th align="right">reasoning</Th>
              <Th align="right">total</Th>
              <Th align="right">API-equiv</Th>
              <Th align="right">elapsed</Th>
              <Th />
            </>
          }
        >
          {visible.map((t) => (
            <LiveRow key={t.id} task={t} />
          ))}
        </Table>
      )}
    </Screen>
  )
}

/**
 * One row, subscribed to one task.
 *
 * This is the whole reason the live store exists outside React: an update to
 * another task does not touch this component at all.
 */
function LiveRow({ task }: { task: TaskSummary }) {
  const taskId = task.id
  const conn = useConnection()
  const live = useLiveTask(taskId)
  const metrics = live?.metrics ?? null
  // The list is authoritative about status; the stream only carries running
  // tasks, so a failed one has no snapshot and would otherwise read "pending".
  const status = task.status === 'running' ? (metrics?.status ?? 'running') : task.status

  return (
    <>
      <Row>
        <Td>
          <button
            type="button"
            className="flex items-center gap-1.5 text-left hover:text-accent"
            onClick={() => navigate('/task', { task: taskId })}
          >
            <StatusDot status={status} />
            {task.name}
          </button>
        </Td>
        <Td>{task.adapter_id}</Td>
        <Td>
          {metrics?.model_id ?? (
            <span
              className="text-text-mute"
              title="No model has been observed for this task yet, so cost cannot be computed."
            >
              unknown
            </span>
          )}
        </Td>
        <Td align="right" numeric>
          {metrics ? (
            <>
              {metrics.requests.is_lower_bound && '≥'}
              {metrics.requests.succeeded}
              {metrics.requests.failed > 0 && (
                <span
                  className="ml-1 text-neg"
                  title="requests that failed and could not be measured"
                >
                  +{metrics.requests.failed}
                </span>
              )}
            </>
          ) : (
            '—'
          )}
        </Td>
        <Td align="right" numeric>
          {metrics ? inputSideTotal(metrics.bands).toLocaleString('en-US') : '—'}
        </Td>
        <Td align="right" numeric>
          {metrics ? metrics.bands.output_total.toLocaleString('en-US') : '—'}
        </Td>
        <Td align="right">
          {metrics ? <TokenCount measured={metrics.reasoning_tokens} showTag={false} /> : '—'}
        </Td>
        <Td align="right">
          {metrics ? <TokenCount measured={metrics.total_tokens} showTag={false} /> : '—'}
        </Td>
        <Td align="right">
          {metrics ? (
            <Money
              measured={metrics.cost.api_equivalent}
              kind="api-equivalent"
              // Live snapshots are pushed in USD: a broadcast has no client, so it
              // cannot know whose currency to convert into. Relabelling the figure
              // here would put a dollar amount under a euro sign.
              currency="USD"
            />
          ) : (
            '—'
          )}
        </Td>
        <Td align="right" numeric>
          {metrics ? formatElapsed(metrics.elapsed_ms) : '—'}
        </Td>
        <Td align="right">
          {status === 'running' && conn && (
            <Button
              variant="danger"
              onClick={() => {
                void stopTask(conn, taskId)
              }}
            >
              Stop
            </Button>
          )}
        </Td>
      </Row>

      {task.failure_detail && (
        <tr>
          <td colSpan={11} className="border-border border-b bg-neg/5 px-3 py-2">
            <div className="text-[11px] text-text-mute">
              {task.adapter_id} exited with an error and said:
            </div>
            {/* The agent's own words, verbatim. Paraphrasing would lose the one
              thing that makes this actionable. */}
            <pre className="mt-1 whitespace-pre-wrap font-mono text-[11px] text-neg leading-relaxed">
              {task.failure_detail}
            </pre>
          </td>
        </tr>
      )}
    </>
  )
}

function formatElapsed(ms: number): string {
  const total = Math.floor(ms / 1000)
  const minutes = Math.floor(total / 60)
  const seconds = total % 60
  return `${minutes}m ${String(seconds).padStart(2, '0')}s`
}
