/**
 * Starting tasks, and comparing the ones that have run.
 *
 * A benchmark here is two or more tasks given the same job, so the form starts
 * one task at a time and the comparison below groups whatever has run. Starting
 * two agents on the same prompt is a matter of submitting the form twice, which
 * is a small enough step that a wizard would add ceremony rather than remove it.
 */

import type { Comparison, TaskSummary } from '@aum/api-contract'
import { useEffect, useState } from 'react'
import { useConnection } from '../../backend/backend-provider'
import { type NewTask, createTask, fetchComparison, fetchTasks } from '../../backend/client'
import { bridge } from '../../platform/bridge'
import { Money, TokenCount } from '../../viz/measurement'
import {
  Button,
  Card,
  Empty,
  Field,
  Row,
  Screen,
  StatusDot,
  Table,
  Td,
  Th,
  inputClass,
} from '../../viz/ui'
import { useCurrency } from '../preferences'
import { navigate, useLocation } from '../router'

export function Benchmarks() {
  const conn = useConnection()
  const [tasks, setTasks] = useState<TaskSummary[]>([])

  useEffect(() => {
    if (!conn) return
    const ac = new AbortController()
    void fetchTasks(conn, ac.signal)
      .then(setTasks)
      .catch(() => {})
    return () => ac.abort()
  }, [conn])

  return (
    <Screen
      title="Benchmarks"
      subtitle="Give two agents the same job and compare what each one spent. Start a task, then start another with the same prompt and a different agent."
    >
      <div className="mb-6 max-w-2xl">
        <NewTaskForm
          onCreated={(task) => {
            // Show it immediately rather than waiting for a refetch: the task
            // exists, and Live Tasks is about to poll for the rest anyway.
            setTasks((previous) => [task, ...previous])
            navigate('/live')
          }}
        />
      </div>

      <ComparisonTable tasks={tasks} />
    </Screen>
  )
}

function NewTaskForm({ onCreated }: { onCreated: (task: TaskSummary) => void }) {
  const conn = useConnection()
  const [form, setForm] = useState<NewTask>({
    name: '',
    adapter_id: 'claude_code',
    working_dir: '',
    prompt: '',
  })
  const [envKeys, setEnvKeys] = useState<string[]>([])
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const submit = () => {
    if (!conn) return
    setBusy(true)
    setError(null)
    createTask(conn, form)
      .then((task) => {
        setForm({ ...form, name: '', prompt: '' })
        onCreated(task)
      })
      .catch((e: unknown) => setError(String(e)))
      .finally(() => setBusy(false))
  }

  return (
    <Card title="New task">
      <div className="flex flex-col gap-3">
        <Field label="Task name" htmlFor="task-name">
          <input
            id="task-name"
            className={inputClass}
            value={form.name}
            placeholder="Implement JWT authentication"
            onChange={(e) => setForm({ ...form, name: e.target.value })}
          />
        </Field>

        <Field label="Agent" htmlFor="task-agent">
          <select
            id="task-agent"
            className={inputClass}
            value={form.adapter_id}
            onChange={(e) => setForm({ ...form, adapter_id: e.target.value })}
          >
            <option value="claude_code">Claude Code</option>
            <option value="codex">Codex</option>
          </select>
        </Field>

        <Field
          label="Working directory"
          hint="The agent runs here. Its usage is attributed by session identity, not by directory, so two tasks may safely share one."
        >
          <div className="flex gap-2">
            <input
              aria-label="Working directory path"
              className={`${inputClass} flex-1`}
              value={form.working_dir}
              placeholder="/Users/you/project"
              onChange={(e) => setForm({ ...form, working_dir: e.target.value })}
            />
            <Button
              onClick={() => {
                void bridge.native.pickDirectory().then((dir) => {
                  if (dir) setForm((f) => ({ ...f, working_dir: dir }))
                })
              }}
            >
              Choose…
            </Button>
          </div>
        </Field>

        <Field label="Prompt" htmlFor="task-prompt">
          <textarea
            id="task-prompt"
            className={`${inputClass} min-h-20 resize-y`}
            value={form.prompt}
            placeholder="What the agent should do"
            onChange={(e) => setForm({ ...form, prompt: e.target.value })}
          />
        </Field>

        <EnvEditor
          keys={envKeys}
          onChange={(keys, entries) => {
            setEnvKeys(keys)
            setForm((f) => ({ ...f, env: entries }))
          }}
        />

        {error && <p className="text-neg">{error}</p>}

        <div>
          <Button
            variant="primary"
            disabled={busy || !form.name || !form.working_dir || !form.prompt}
            onClick={submit}
          >
            {busy ? 'Starting…' : 'Start task'}
          </Button>
        </div>
      </div>
    </Card>
  )
}

/**
 * Environment variables for the agent.
 *
 * Values are write-only: once entered they are never displayed again, never
 * stored, and never returned by the API. Environment variables passed to an AI
 * agent routinely contain API keys, and a monitoring tool that kept them would
 * be a worse liability than the problem it solves.
 */
function EnvEditor({
  keys,
  onChange,
}: {
  keys: string[]
  onChange: (keys: string[], entries: [string, string][]) => void
}) {
  const [entries, setEntries] = useState<[string, string][]>([])
  const [name, setName] = useState('')
  const [value, setValue] = useState('')

  const add = () => {
    if (!name) return
    const next: [string, string][] = [...entries, [name, value]]
    setEntries(next)
    onChange([...keys, name], next)
    setName('')
    setValue('')
  }

  return (
    <Field
      label="Environment variables"
      hint="Passed to the agent and never stored. Values are not shown again once added."
    >
      <div className="flex gap-2">
        <input
          aria-label="Environment variable name"
          className={`${inputClass} flex-1`}
          value={name}
          placeholder="NAME"
          onChange={(e) => setName(e.target.value)}
        />
        <input
          aria-label="Environment variable value"
          className={`${inputClass} flex-1`}
          value={value}
          type="password"
          placeholder="value"
          onChange={(e) => setValue(e.target.value)}
        />
        <Button onClick={add} disabled={!name}>
          Add
        </Button>
      </div>
      {keys.length > 0 && (
        <div className="mt-1 flex flex-wrap gap-1">
          {keys.map((k) => (
            <span
              key={k}
              className="rounded border border-border bg-surface-2 px-1.5 py-0.5 font-mono text-[10px] text-text-dim"
              title="The value was set and is not displayed"
            >
              {k} = ••••
            </span>
          ))}
        </div>
      )}
    </Field>
  )
}

/**
 * Everything that has run, side by side.
 *
 * Two decisions carry most of the honesty here.
 *
 * **The selection lives in the URL.** `#/benchmarks?tasks=a,b&norm=1` is the
 * single most useful link this application produces — it is what gets pasted
 * into a message when someone asks which agent was cheaper — so it has to
 * survive a reload, a backend restart, and being sent to somebody else.
 * Currency deliberately stays out of it: that is the reader's setting, and a
 * shared link should not reformat their screen.
 *
 * **Normalisation and the caveats come from the backend.** "Cost per 1,000
 * output tokens" divides money, and money arrives as a decimal string exactly
 * so that nobody divides it as a float. Whether these rows are comparable at
 * all is also a fact about the data rather than a fixed sentence, so the
 * backend computes it from the rows and the table prints what it says.
 */
function ComparisonTable({ tasks }: { tasks: TaskSummary[] }) {
  const conn = useConnection()
  const currency = useCurrency()
  const location = useLocation()
  const [comparison, setComparison] = useState<Comparison | null>(null)
  const [error, setError] = useState<string | null>(null)

  const selectedParam = location.params.get('tasks')
  const normalize = location.params.get('norm') === '1'

  // With nothing selected, compare everything — the useful default when two
  // tasks have just been started and there is nothing to disambiguate.
  const selected =
    selectedParam === null
      ? tasks.map((t) => t.id)
      : selectedParam.split(',').filter((id) => id.length > 0)

  const key = selected.join(',')

  useEffect(() => {
    if (!conn || key.length === 0) {
      setComparison(null)
      return
    }
    const ac = new AbortController()
    fetchComparison(conn, key.split(','), { currency, normalize }, ac.signal)
      .then((c) => {
        setComparison(c)
        setError(null)
      })
      .catch((e: unknown) => {
        if (!ac.signal.aborted) setError(String(e))
      })
    return () => ac.abort()
  }, [conn, key, currency, normalize])

  const setSelection = (ids: string[]) => {
    const params: Record<string, string> = { tasks: ids.join(',') }
    if (normalize) params.norm = '1'
    navigate('/benchmarks', params)
  }

  const toggle = (id: string) => {
    setSelection(selected.includes(id) ? selected.filter((x) => x !== id) : [...selected, id])
  }

  if (tasks.length === 0) {
    return <Empty>No tasks yet. Start one above and it will appear here.</Empty>
  }

  return (
    <>
      <div className="mb-2 flex items-center gap-3">
        <h2 className="font-medium text-[11px] text-text-mute uppercase tracking-wide">
          Comparison
        </h2>
        <label className="flex items-center gap-1.5 text-[11px] text-text-dim">
          <input
            type="checkbox"
            checked={normalize}
            onChange={(e) => {
              const params: Record<string, string> = { tasks: selected.join(',') }
              if (e.target.checked) params.norm = '1'
              navigate('/benchmarks', params)
            }}
          />
          per 1,000 output tokens
        </label>
        {selectedParam !== null && (
          <Button onClick={() => navigate('/benchmarks')}>Compare all</Button>
        )}
      </div>

      {error && <p className="mb-2 text-neg">{error}</p>}

      <Table
        head={
          <>
            <Th>compare</Th>
            <Th>task</Th>
            <Th>agent</Th>
            <Th>model</Th>
            <Th align="right">requests</Th>
            <Th align="right">{normalize ? 'tokens / 1k out' : 'total tokens'}</Th>
            <Th align="right">reasoning</Th>
            <Th align="right">{normalize ? 'API-equiv / 1k out' : 'API-equivalent'}</Th>
            <Th align="right">billed</Th>
            <Th align="right">duration</Th>
          </>
        }
      >
        {tasks.map((task) => {
          const row = comparison?.rows.find((r) => r.task_id === task.id)
          const m = row?.metrics
          const n = row?.normalized
          const included = selected.includes(task.id)

          return (
            <Row key={task.id}>
              <Td>
                <input
                  type="checkbox"
                  checked={included}
                  onChange={() => toggle(task.id)}
                  aria-label={`Include ${task.name} in the comparison`}
                />
              </Td>
              <Td>
                <span className="flex items-center gap-1.5">
                  <StatusDot status={task.status} />
                  {task.name}
                </span>
              </Td>
              <Td>{task.adapter_id}</Td>
              <Td>{m?.model_id ?? <span className="text-text-mute">unknown</span>}</Td>
              <Td align="right" numeric>
                {m ? `${m.requests.is_lower_bound ? '≥' : ''}${m.requests.succeeded}` : '—'}
              </Td>
              <Td align="right">
                {normalize ? (
                  // No output yet means nothing to divide by. Showing 0 would
                  // read as "this task is free", which is a different claim.
                  n ? (
                    <TokenCount measured={n.total_tokens} showTag={false} />
                  ) : (
                    <span className="text-text-mute" title="No output produced yet.">
                      —
                    </span>
                  )
                ) : m ? (
                  <TokenCount measured={m.total_tokens} showTag={false} />
                ) : (
                  '—'
                )}
              </Td>
              <Td align="right">
                {m ? <TokenCount measured={m.reasoning_tokens} showTag={false} /> : '—'}
              </Td>
              <Td align="right">
                {normalize ? (
                  n ? (
                    <Money measured={n.cost} kind="api-equivalent" currency={currency} />
                  ) : (
                    '—'
                  )
                ) : m ? (
                  <Money
                    measured={m.cost.api_equivalent}
                    kind="api-equivalent"
                    currency={currency}
                  />
                ) : (
                  '—'
                )}
              </Td>
              <Td align="right">
                {m ? (
                  <Money measured={m.cost.actual_billed} kind="billed" currency={currency} />
                ) : (
                  '—'
                )}
              </Td>
              <Td align="right" numeric>
                {m ? `${Math.round(m.elapsed_ms / 1000)}s` : '—'}
              </Td>
            </Row>
          )
        })}
      </Table>

      {comparison && comparison.caveats.length > 0 && (
        <Card title="Why these rows are not directly comparable" className="mt-3 max-w-3xl">
          <ul className="flex flex-col gap-1.5 text-text-dim leading-relaxed">
            {comparison.caveats.map((caveat) => (
              <li key={caveat}>{caveat}</li>
            ))}
          </ul>
        </Card>
      )}

      <p className="mt-3 max-w-3xl text-[11px] text-text-mute leading-relaxed">
        Duration is wall-clock and includes tool execution, retry backoff and waiting — it is not a
        speed measurement, which is why there is no per-token version of it. Neither agent's usage
        is billed per token under a subscription, so "billed" is genuinely unknown rather than zero.
      </p>
    </>
  )
}
