/**
 * Starting tasks, and comparing the ones that have run.
 *
 * A benchmark here is two or more tasks given the same job, so the form starts
 * one task at a time and the comparison below groups whatever has run. Starting
 * two agents on the same prompt is a matter of submitting the form twice, which
 * is a small enough step that a wizard would add ceremony rather than remove it.
 */

import type { TaskSummary } from '@aum/api-contract'
import { useEffect, useState } from 'react'
import { useConnection } from '../../backend/backend-provider'
import { type NewTask, createTask, fetchTaskMetrics, fetchTasks } from '../../backend/client'
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
import { navigate } from '../router'

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

/** Everything that has run, side by side. */
function ComparisonTable({ tasks }: { tasks: TaskSummary[] }) {
  const conn = useConnection()
  const [metrics, setMetrics] = useState<
    Record<string, Awaited<ReturnType<typeof fetchTaskMetrics>>>
  >({})

  useEffect(() => {
    if (!conn || tasks.length === 0) return
    const ac = new AbortController()
    for (const task of tasks.slice(0, 25)) {
      void fetchTaskMetrics(conn, task.id, ac.signal)
        .then((m) => setMetrics((prev) => ({ ...prev, [task.id]: m })))
        .catch(() => {})
    }
    return () => ac.abort()
  }, [conn, tasks])

  if (tasks.length === 0) {
    return <Empty>No tasks yet. Start one above and it will appear here.</Empty>
  }

  return (
    <>
      <h2 className="mb-2 font-medium text-[11px] text-text-mute uppercase tracking-wide">
        Comparison
      </h2>
      <Table
        head={
          <>
            <Th>task</Th>
            <Th>agent</Th>
            <Th>model</Th>
            <Th align="right">requests</Th>
            <Th align="right">total tokens</Th>
            <Th align="right">reasoning</Th>
            <Th align="right">API-equivalent</Th>
            <Th align="right">billed</Th>
            <Th align="right">duration</Th>
          </>
        }
      >
        {tasks.map((task) => {
          const m = metrics[task.id]
          return (
            <Row key={task.id}>
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
                {m ? <TokenCount measured={m.total_tokens} showTag={false} /> : '—'}
              </Td>
              <Td align="right">
                {m ? <TokenCount measured={m.reasoning_tokens} showTag={false} /> : '—'}
              </Td>
              <Td align="right">
                {m ? (
                  <Money measured={m.cost.api_equivalent} kind="api-equivalent" currency="USD" />
                ) : (
                  '—'
                )}
              </Td>
              <Td align="right">
                {m ? <Money measured={m.cost.actual_billed} kind="billed" currency="USD" /> : '—'}
              </Td>
              <Td align="right" numeric>
                {m ? `${Math.round(m.elapsed_ms / 1000)}s` : '—'}
              </Td>
            </Row>
          )
        })}
      </Table>

      <p className="mt-3 max-w-3xl text-[11px] text-text-mute leading-relaxed">
        Columns are only comparable where both agents report the same thing. Reasoning tokens are
        reported by Codex and not by Claude Code, and neither agent's usage is billed per token
        under a subscription — so "billed" is genuinely unknown rather than zero.
      </p>
    </>
  )
}
