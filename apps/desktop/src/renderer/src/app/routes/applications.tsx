/**
 * Applications, and what each one can actually tell us.
 *
 * The matrix is not written down anywhere in this file. It comes from the
 * backend, which derives it by running the real parsers over each application's
 * own recent files. Every claim carries the evidence that produced it, and a
 * capability that has not been observed shows as `?` rather than as yes.
 */

import type { AdapterDescriptor, CapabilityState, DailyTotal } from '@aum/api-contract'
import { useEffect, useState } from 'react'
import { useConnection } from '../../backend/backend-provider'
import { fetchAdapters } from '../../backend/client'
import { TokenCount } from '../../viz/measurement'
import { Card, Empty, Screen } from '../../viz/ui'

const CAPABILITY_LABELS: Record<string, string> = {
  exact_token_counts: 'Exact token counts',
  cache_read_tokens: 'Cache read tokens',
  cache_write_tokens: 'Cache write tokens',
  cache_ttl_breakdown: 'Cache TTL breakdown',
  reasoning_tokens: 'Reasoning tokens',
  model_identity: 'Model identity',
  per_request_latency: 'Per-request latency',
  provider_reported_cost: 'Cost reported by the agent',
  actual_billed_cost: 'Actual billed cost',
  sub_agent_attribution: 'Sub-agent attribution',
  launch_with_pinned_session: 'Launch with pinned session',
  failure_visibility: 'Failure visibility',
}

export function Applications() {
  const conn = useConnection()
  const [adapters, setAdapters] = useState<AdapterDescriptor[] | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (!conn) return
    const ac = new AbortController()
    fetchAdapters(conn, ac.signal)
      .then(setAdapters)
      .catch((e: unknown) => {
        if (!ac.signal.aborted) setError(String(e))
      })
    return () => ac.abort()
  }, [conn])

  return (
    <Screen
      title="Applications"
      subtitle="What each application on this machine can actually report. Determined by reading its own files, not from a list — so it stays true when these tools change."
    >
      {error && <p className="mb-4 text-neg">{error}</p>}
      {!adapters && <Empty>Checking what each application reports…</Empty>}

      <div className="flex max-w-4xl flex-col gap-4">
        {adapters?.map((a) => (
          <AdapterCard key={a.id} adapter={a} />
        ))}
      </div>
    </Screen>
  )
}

function AdapterCard({ adapter }: { adapter: AdapterDescriptor }) {
  const stateLabel = {
    ready: 'reporting usage',
    detected: 'installed',
    not_installed: 'not found',
    error: 'error',
  }[adapter.state]

  const stateColour = {
    ready: 'text-exact',
    detected: 'text-warn',
    not_installed: 'text-text-mute',
    error: 'text-neg',
  }[adapter.state]

  return (
    <Card>
      <div className="mb-3 flex items-baseline justify-between gap-3">
        <h2 className="font-medium text-[13px]">{adapter.display_name}</h2>
        <span className={`text-[11px] ${stateColour}`}>{stateLabel}</span>
      </div>

      {adapter.executable_path && (
        <p className="mb-3 font-mono text-[10px] text-text-faint">{adapter.executable_path}</p>
      )}

      <div className="mb-3 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
        {adapter.capabilities.map(([name, state]) => (
          <Capability key={name} name={name} state={state} />
        ))}
      </div>

      {adapter.daily_total && <DailyTotalPanel total={adapter.daily_total} />}

      {adapter.notes.map((note) => (
        <p key={note} className="mt-1.5 text-[11px] text-text-mute leading-relaxed">
          {note}
        </p>
      ))}
    </Card>
  )
}

/**
 * The one figure an application reports when it reports nothing per request.
 *
 * Rendered apart from the capability matrix and apart from every other number
 * in the product, because it is a different kind of thing: a whole-application
 * running total that cannot be attributed to a task or priced. The scope
 * sentence sits beside the number rather than in a tooltip, so a screenshot of
 * it cannot claim more than the number supports.
 */
function DailyTotalPanel({ total }: { total: DailyTotal }) {
  // Yesterday first: today's is still climbing, and the finished days are the
  // ones worth comparing against each other.
  const earlier = total.history.filter((d) => d.day !== total.day).slice(0, 7)

  return (
    <div className="mt-3 rounded border border-border bg-surface-2 px-3 py-2">
      <div className="flex items-baseline justify-between gap-3">
        <span className="text-[11px] text-text-mute">tokens on {total.day}</span>
        <span className="numeric text-[15px]">
          <TokenCount measured={total.tokens} />
        </span>
      </div>

      <p className="mt-1.5 text-[10px] text-text-mute leading-relaxed">{total.scope}</p>

      {earlier.length > 0 && (
        <div className="mt-2 border-border border-t pt-2">
          <div className="mb-1 text-[10px] text-text-faint">
            earlier days, kept here because the application discards them at midnight
          </div>
          <ul className="flex flex-col gap-0.5">
            {earlier.map((d) => (
              <li key={d.day} className="flex justify-between text-[11px] text-text-dim">
                <span className="font-mono">{d.day}</span>
                <span className="numeric">{d.tokens.toLocaleString('en-US')}</span>
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  )
}

function Capability({ name, state }: { name: string; state: CapabilityState }) {
  // Four visually distinct marks, differing in shape and text as well as
  // colour, so the matrix survives being printed in greyscale.
  const { mark, colour, detail } = describe(state)

  return (
    <>
      <div className={`text-[11px] ${colour}`} title={detail}>
        <span className="mr-1.5 inline-block w-6 font-mono">{mark}</span>
        {CAPABILITY_LABELS[name] ?? name}
      </div>
      <div className="truncate text-[11px] text-text-faint" title={detail}>
        {detail}
      </div>
    </>
  )
}

function describe(state: CapabilityState): {
  mark: string
  colour: string
  detail: string
} {
  switch (state.state) {
    case 'supported':
      return { mark: 'yes', colour: 'text-exact', detail: state.evidence }
    case 'degraded':
      return { mark: '~', colour: 'text-warn', detail: state.caveat }
    case 'unsupported':
      return { mark: 'no', colour: 'text-text-mute', detail: state.reason }
    case 'unknown':
      // Never rendered as yes. "Not checked" and "not supported" are different
      // statements and the matrix keeps them apart.
      return { mark: '?', colour: 'text-text-faint', detail: state.reason }
  }
}
