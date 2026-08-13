/**
 * A backend that is not the Rust one.
 *
 * This exists for three reasons, in order of importance:
 *
 *  1. **It proves the boundary is real.** The desktop app runs against this
 *     unmodified. If a ~250-line TypeScript process can satisfy the contract,
 *     so can a Go or C# one — which is the stated requirement, demonstrated
 *     rather than asserted.
 *  2. **It is the load generator.** Real agents will not produce 200 events a
 *     second on demand; this will, so the UI's re-render behaviour can be
 *     measured rather than assumed.
 *  3. **It emits the awkward cases on purpose** — unavailable reasoning tokens,
 *     partial aggregates, unknown models with no price, subscription-billed
 *     costs. These are the states the UI most needs to render honestly and the
 *     ones real data produces least predictably.
 *
 * Everything it reports is synthetic and labelled as such. It is a test double,
 * never a source of numbers the product shows as real.
 *
 * Usage:
 *   AUM_TOKEN=<token> bun run tools/fake-sidecar/index.ts [--tasks N] [--rate HZ]
 */

const CONTRACT_VERSION = '1.0.0'

interface Args {
  tasks: number
  rate: number
}

function parseArgs(): Args {
  const argv = process.argv.slice(2)
  const read = (flag: string, fallback: number): number => {
    const i = argv.indexOf(flag)
    if (i === -1) return fallback
    const n = Number(argv[i + 1])
    return Number.isFinite(n) ? n : fallback
  }
  return { tasks: read('--tasks', 3), rate: read('--rate', 4) }
}

const args = parseArgs()
const token = process.env.AUM_TOKEN
if (!token || token.length < 32) {
  console.error('AUM_TOKEN must be set to at least 32 characters (same rule as the real sidecar).')
  process.exit(1)
}
const allowedOrigin = process.env.AUM_ALLOWED_ORIGIN ?? 'app://local'
const streamEpoch = crypto.randomUUID()
const startedAt = Date.now()

// ── synthetic task state ─────────────────────────────────────────────────────

interface FakeTask {
  id: string
  name: string
  adapter: string
  model: string | null
  reportsReasoning: boolean
  hasPrice: boolean
  hasGap: boolean
  bands: {
    input_fresh: number
    cache_read: number
    cache_write_5m: number
    cache_write_1h: number
    cache_write_unspecified: number
    output_total: number
    reasoning: number | null
  }
  requests: number
  unmeasured: number
  startedAt: number
}

const ADAPTERS = ['claude_code', 'codex'] as const

function makeTask(i: number): FakeTask {
  const adapter = ADAPTERS[i % 2] ?? 'claude_code'
  const isCodex = adapter === 'codex'
  return {
    id: crypto.randomUUID(),
    name: `Synthetic task ${i + 1}`,
    adapter,
    // Every fourth task has no model yet, which is a real state: a tailer that
    // starts mid-file has not seen a model declaration, so cost is unknowable.
    model: i % 4 === 3 ? null : isCodex ? 'gpt-5.6-sol' : 'claude-opus-5',
    // Codex reports reasoning tokens; Claude Code does not. The UI must render
    // the difference as absence, not as zero.
    reportsReasoning: isCodex,
    // `gpt-5.6-sol` is in the real corpus and in no public price list, so cost
    // must come out Unavailable rather than as a substituted rate.
    hasPrice: !isCodex,
    // Every third task has an unmeasured request, forcing a Partial aggregate.
    hasGap: i % 3 === 2,
    bands: {
      input_fresh: 0,
      cache_read: 0,
      cache_write_5m: 0,
      cache_write_1h: 0,
      cache_write_unspecified: 0,
      output_total: 0,
      reasoning: isCodex ? 0 : null,
    },
    requests: 0,
    unmeasured: 0,
    startedAt: Date.now(),
  }
}

const tasks: FakeTask[] = Array.from({ length: args.tasks }, (_, i) => makeTask(i))

let seq = 0
const subscribers = new Set<(chunk: string) => void>()

function publish(taskId: string | null, payload: Record<string, unknown>): void {
  seq += 1
  const envelope = {
    seq,
    stream_epoch: streamEpoch,
    ts: new Date().toISOString(),
    ...(taskId ? { task_id: taskId } : {}),
    ...payload,
  }
  const frame = `id: ${seq}\nevent: ${String(payload.type)}\ndata: ${JSON.stringify(envelope)}\n\n`
  for (const send of subscribers) send(frame)
}

function advance(task: FakeTask): void {
  const fresh = 200 + Math.floor(Math.random() * 3_000)
  const output = 50 + Math.floor(Math.random() * 800)
  task.bands.input_fresh += fresh
  task.bands.cache_read += Math.floor(Math.random() * 20_000)
  task.bands.cache_write_1h += Math.floor(Math.random() * 2_000)
  task.bands.output_total += output
  if (task.reportsReasoning && task.bands.reasoning !== null) {
    task.bands.reasoning += Math.floor(output * 0.2)
  }
  task.requests += 1

  // Occasionally a request completes with no usage reported at all.
  if (task.hasGap && task.requests % 7 === 0) task.unmeasured += 1

  publish(task.id, {
    type: 'usage_delta',
    request_id: crypto.randomUUID(),
    model_id: task.model ?? 'unknown',
    bands: {
      input_fresh: fresh,
      cache_read: 0,
      cache_write_5m: 0,
      cache_write_1h: 0,
      cache_write_unspecified: 0,
      output_total: output,
      reasoning: task.reportsReasoning ? Math.floor(output * 0.2) : null,
    },
    measurement_source: 'provider_reported',
  })
}

function metricsFor(task: FakeTask): Record<string, unknown> {
  const total =
    task.bands.input_fresh +
    task.bands.cache_read +
    task.bands.cache_write_5m +
    task.bands.cache_write_1h +
    task.bands.cache_write_unspecified +
    task.bands.output_total

  const measured = task.requests
  const totalContributors = task.requests + task.unmeasured

  const totalTokens =
    task.unmeasured > 0
      ? { value: total, accuracy: { kind: 'partial', measured, total: totalContributors } }
      : { value: total, accuracy: { kind: 'exact', source: 'provider_reported' } }

  const reasoning = task.reportsReasoning
    ? { value: task.bands.reasoning, accuracy: { kind: 'exact', source: 'provider_reported' } }
    : {
        value: null,
        accuracy: {
          kind: 'unavailable',
          reason: {
            kind: 'not_reported_by_provider',
            field: 'reasoning_tokens',
            detail: 'Claude Code does not report a reasoning-token count.',
          },
        },
      }

  const apiEquivalent =
    task.model === null
      ? {
          value: null,
          accuracy: {
            kind: 'unavailable',
            reason: {
              kind: 'model_unknown',
              detail: 'No model has been observed for this task yet.',
            },
          },
        }
      : task.hasPrice
        ? {
            value: (total * 0.000012).toFixed(6),
            accuracy: { kind: 'calculated', source: 'application_telemetry' },
          }
        : {
            value: null,
            accuracy: {
              kind: 'unavailable',
              reason: { kind: 'no_pricing_for_model', model_id: task.model },
            },
          }

  const latencyUnavailable = {
    value: null,
    accuracy: {
      kind: 'unavailable',
      reason: {
        kind: 'requires_capture_level',
        level: 'the local proxy',
        detail: 'Transcripts contain no latency field.',
      },
    },
  }

  return {
    task_id: task.id,
    status: 'running',
    bands: task.bands,
    total_tokens: totalTokens,
    reasoning_tokens: reasoning,
    requests: {
      succeeded: task.requests,
      failed: 0,
      // Codex does not expose retries at all; reporting 0 would make it look
      // flawless when it is merely opaque.
      retries: task.adapter === 'codex' ? null : 0,
      is_lower_bound: task.unmeasured > 0,
    },
    elapsed_ms: Date.now() - task.startedAt,
    model_id: task.model,
    cost: {
      currency: 'USD',
      api_equivalent: apiEquivalent,
      provider_reported: {
        value: null,
        accuracy: {
          kind: 'unavailable',
          reason: {
            kind: 'not_reported_by_provider',
            field: 'cost',
            detail: 'This agent does not report a cost figure.',
          },
        },
      },
      actual_billed: {
        value: null,
        accuracy: {
          kind: 'unavailable',
          reason: {
            kind: 'subscription_billed',
            plan: task.adapter === 'codex' ? 'plus' : 'team',
          },
        },
      },
    },
    latency: {
      average_ms: latencyUnavailable,
      median_ms: latencyUnavailable,
      p95_ms: latencyUnavailable,
      time_to_first_token_ms: latencyUnavailable,
      output_tokens_per_sec: latencyUnavailable,
    },
  }
}

function taskSummary(task: FakeTask): Record<string, unknown> {
  return {
    id: task.id,
    benchmark_id: null,
    name: task.name,
    adapter_id: task.adapter,
    status: 'running',
    binding: { mode: 'launched_stdout', pid: process.pid },
    working_dir: '/synthetic',
    model_id: task.model,
    started_at: new Date(task.startedAt).toISOString(),
    ended_at: null,
  }
}

// ── HTTP ─────────────────────────────────────────────────────────────────────

function cors(origin: string | null): Record<string, string> {
  return {
    'Access-Control-Allow-Origin': allowedOrigin,
    'Access-Control-Allow-Headers': 'authorization, content-type, accept, last-event-id',
    'Access-Control-Allow-Methods': 'GET, POST, DELETE, OPTIONS',
    Vary: 'Origin',
    ...(origin === null ? {} : {}),
  }
}

function authorised(req: Request): boolean {
  const presented = req.headers.get('authorization')?.replace(/^Bearer /, '')
  if (presented !== token) return false
  const origin = req.headers.get('origin')
  if (origin !== null && origin !== allowedOrigin) return false
  return true
}

const server = Bun.serve({
  hostname: '127.0.0.1',
  port: 0,
  idleTimeout: 0,
  fetch(req) {
    const url = new URL(req.url)
    const origin = req.headers.get('origin')
    const headers = cors(origin)

    if (req.method === 'OPTIONS') return new Response(null, { status: 204, headers })

    if (url.pathname === '/v1/health') {
      return Response.json({ status: 'ok', uptime_ms: Date.now() - startedAt }, { headers })
    }

    if (!authorised(req)) return new Response('unauthorised', { status: 401, headers })

    if (url.pathname === '/v1/meta') {
      return Response.json(
        {
          contract_version: CONTRACT_VERSION,
          impl_name: 'aum-sidecar-fake-typescript',
          impl_version: '0.1.0',
          stream_epoch: streamEpoch,
          capabilities: ['events', 'synthetic'],
        },
        { headers },
      )
    }

    if (url.pathname === '/v1/tasks') {
      return Response.json(tasks.map(taskSummary), { headers })
    }

    if (url.pathname === '/v1/events') {
      const stream = new ReadableStream({
        start(controller) {
          const encoder = new TextEncoder()
          const send = (chunk: string) => {
            try {
              controller.enqueue(encoder.encode(chunk))
            } catch {
              subscribers.delete(send)
            }
          }
          subscribers.add(send)
          send(': connected\n\n')
          for (const task of tasks) {
            send(
              `event: task_created\ndata: ${JSON.stringify({
                seq: 0,
                stream_epoch: streamEpoch,
                ts: new Date().toISOString(),
                type: 'task_created',
                task: taskSummary(task),
              })}\n\n`,
            )
          }
        },
        cancel() {
          subscribers.clear()
        },
      })
      return new Response(stream, {
        headers: {
          ...headers,
          'Content-Type': 'text/event-stream',
          'Cache-Control': 'no-cache',
          Connection: 'keep-alive',
        },
      })
    }

    return new Response('not found', { status: 404, headers })
  },
})

// Handshake: byte-identical in shape to the Rust sidecar's, because that is the
// entire point of this process existing.
process.stdout.write(
  `${JSON.stringify({
    kind: 'aum.handshake',
    protocol: 1,
    port: server.port,
    pid: process.pid,
    contract_version: CONTRACT_VERSION,
    impl_name: 'aum-sidecar-fake-typescript',
    impl_version: '0.1.0',
    started_at: new Date().toISOString(),
  })}\n`,
)

console.error(
  `[fake-sidecar] SYNTHETIC BACKEND on 127.0.0.1:${server.port} — ` +
    `${tasks.length} tasks at ${args.rate} Hz. All numbers here are fabricated.`,
)

setInterval(() => {
  for (const task of tasks) advance(task)
}, 1000 / Math.max(args.rate, 1))

setInterval(() => {
  for (const task of tasks) {
    publish(task.id, { type: 'metrics_snapshot', metrics: metricsFor(task) })
  }
}, 1000)

setInterval(() => {
  if (subscribers.size > 0) publish(null, { type: 'heartbeat', lag_ms: 0 })
}, 10_000)

// Same orphan discipline as the real sidecar: exit when the host does.
process.stdin.on('end', () => process.exit(0))
process.stdin.resume()
