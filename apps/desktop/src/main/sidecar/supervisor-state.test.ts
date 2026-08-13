import { describe, expect, it } from 'vitest'
import {
  BACKOFF_MS,
  type Effect,
  MAX_ATTEMPTS,
  type SupervisorEvent,
  type SupervisorState,
  backoffFor,
  initialState,
  next,
} from './supervisor-state'

function drive(
  events: SupervisorEvent[],
  from: SupervisorState = initialState,
): { state: SupervisorState; effects: Effect[] } {
  let state = from
  const effects: Effect[] = []
  for (const e of events) {
    const [s, fx] = next(state, e)
    state = s
    effects.push(...fx)
  }
  return { state, effects }
}

const goodHandshake: SupervisorEvent = {
  type: 'handshake',
  port: 51234,
  pid: 4711,
  contractVersion: '1.0.0',
  token: 'tok',
}

describe('happy path', () => {
  it('reaches ready and exposes a loopback base URL', () => {
    const { state } = drive([{ type: 'start' }, { type: 'spawned', pid: 1 }, goodHandshake])
    expect(state.phase).toBe('ready')
    expect(state.connection?.baseUrl).toBe('http://127.0.0.1:51234')
    expect(state.generation).toBe(1)
  })

  it('starts health checks only once ready', () => {
    const { effects } = drive([{ type: 'start' }, { type: 'spawned', pid: 1 }, goodHandshake])
    expect(effects).toContainEqual({ type: 'start-health-checks' })
  })

  it('bumps the generation on every successful start', () => {
    const first = drive([{ type: 'start' }, { type: 'spawned', pid: 1 }, goodHandshake])
    const second = drive(
      [
        { type: 'process-exit', code: 1, signal: null, stderrTail: '' },
        { type: 'backoff-elapsed' },
        { type: 'spawned', pid: 2 },
        goodHandshake,
      ],
      first.state,
    )
    // A changed generation is the renderer's signal to discard live state
    // rather than replay a previous backend's numbers into a new one.
    expect(second.state.generation).toBe(2)
  })

  it('ignores a redundant start while already running', () => {
    const ready = drive([{ type: 'start' }, { type: 'spawned', pid: 1 }, goodHandshake])
    const again = drive([{ type: 'start' }], ready.state)
    expect(again.effects).toEqual([])
    expect(again.state.generation).toBe(1)
  })
})

describe('contract mismatch is terminal', () => {
  it('refuses a different major version instead of degrading', () => {
    const { state } = drive([
      { type: 'start' },
      { type: 'spawned', pid: 1 },
      { ...goodHandshake, contractVersion: '2.0.0' },
    ])
    expect(state.phase).toBe('version-mismatch')
    expect(state.connection).toBeNull()
  })

  it('does not schedule a retry for a mismatch', () => {
    const { effects } = drive([
      { type: 'start' },
      { type: 'spawned', pid: 1 },
      { ...goodHandshake, contractVersion: '2.0.0' },
    ])
    expect(effects.some((e) => e.type === 'schedule-backoff')).toBe(false)
  })

  it('accepts a compatible minor version', () => {
    const { state } = drive([
      { type: 'start' },
      { type: 'spawned', pid: 1 },
      { ...goodHandshake, contractVersion: '1.7.3' },
    ])
    expect(state.phase).toBe('ready')
  })

  it('stays in version-mismatch when the killed process exits', () => {
    const mismatched = drive([
      { type: 'start' },
      { type: 'spawned', pid: 1 },
      { ...goodHandshake, contractVersion: '2.0.0' },
    ])
    const after = drive(
      [{ type: 'process-exit', code: 0, signal: 'SIGTERM', stderrTail: '' }],
      mismatched.state,
    )
    expect(after.state.phase).toBe('version-mismatch')
  })
})

describe('restart and backoff', () => {
  it('backs off with increasing delays', () => {
    expect(backoffFor(0)).toBe(BACKOFF_MS[0])
    expect(backoffFor(1)).toBe(BACKOFF_MS[1])
  })

  it('caps the backoff rather than growing without bound', () => {
    expect(backoffFor(99)).toBe(BACKOFF_MS[BACKOFF_MS.length - 1])
  })

  it('gives up after MAX_ATTEMPTS instead of looping forever', () => {
    // A broken binary restarted forever produces a hung UI and a loud fan.
    let state = initialState
    for (let i = 0; i < MAX_ATTEMPTS; i++) {
      state = drive(
        [
          { type: 'start' },
          { type: 'spawned', pid: 1 },
          { type: 'process-exit', code: 1, signal: null, stderrTail: 'boom' },
          { type: 'backoff-elapsed' },
        ],
        state,
      ).state
    }
    expect(state.phase).toBe('failed')
  })

  it('stops scheduling retries once failed', () => {
    let state = initialState
    for (let i = 0; i < MAX_ATTEMPTS; i++) {
      state = drive(
        [
          { type: 'spawned', pid: 1 },
          { type: 'process-exit', code: 1, signal: null, stderrTail: '' },
        ],
        state,
      ).state
    }
    const after = drive([{ type: 'backoff-elapsed' }], state)
    expect(after.effects.some((e) => e.type === 'spawn')).toBe(false)
  })

  it('a manual retry clears the attempt budget', () => {
    let state = initialState
    for (let i = 0; i < MAX_ATTEMPTS; i++) {
      state = drive(
        [
          { type: 'spawned', pid: 1 },
          { type: 'process-exit', code: 1, signal: null, stderrTail: '' },
        ],
        state,
      ).state
    }
    expect(state.phase).toBe('failed')
    const retried = drive([{ type: 'retry-requested' }], state)
    expect(retried.state.phase).toBe('spawning')
    expect(retried.state.attempt).toBe(0)
  })

  it('carries the stderr tail into the failure detail so the user sees the cause', () => {
    const { state } = drive([
      { type: 'start' },
      { type: 'spawned', pid: 1 },
      { type: 'process-exit', code: 1, signal: null, stderrTail: 'AUM_TOKEN is not set' },
    ])
    expect(state.detail).toContain('AUM_TOKEN is not set')
  })

  it('resets the attempt counter after a sustained healthy period', () => {
    const crashed = drive([
      { type: 'start' },
      { type: 'spawned', pid: 1 },
      { type: 'process-exit', code: 1, signal: null, stderrTail: '' },
      { type: 'backoff-elapsed' },
      { type: 'spawned', pid: 2 },
      goodHandshake,
    ])
    expect(crashed.state.attempt).toBe(1)
    const later = drive([{ type: 'tick', now: Date.now() + 120_000 }], crashed.state)
    expect(later.state.attempt).toBe(0)
  })
})

describe('health checks', () => {
  it('tolerates isolated failures without changing phase', () => {
    const ready = drive([{ type: 'start' }, { type: 'spawned', pid: 1 }, goodHandshake])
    const after = drive([{ type: 'health-fail', detail: 'timeout' }], ready.state)
    expect(after.state.phase).toBe('ready')
  })

  it('degrades after repeated failures but does not restart', () => {
    // The sidecar may be mid-GC or blocked on a slow SQLite write. Killing it
    // would turn a hiccup into lost data.
    const ready = drive([{ type: 'start' }, { type: 'spawned', pid: 1 }, goodHandshake])
    const after = drive(
      [
        { type: 'health-fail', detail: 'timeout' },
        { type: 'health-fail', detail: 'timeout' },
        { type: 'health-fail', detail: 'timeout' },
      ],
      ready.state,
    )
    expect(after.state.phase).toBe('degraded')
    expect(after.effects.some((e) => e.type === 'kill')).toBe(false)
    expect(after.state.connection).not.toBeNull()
  })

  it('recovers from degraded when health returns', () => {
    const ready = drive([{ type: 'start' }, { type: 'spawned', pid: 1 }, goodHandshake])
    const degraded = drive(
      [
        { type: 'health-fail', detail: 'x' },
        { type: 'health-fail', detail: 'x' },
        { type: 'health-fail', detail: 'x' },
      ],
      ready.state,
    )
    const recovered = drive([{ type: 'health-ok' }], degraded.state)
    expect(recovered.state.phase).toBe('ready')
    // Recovery must not bump the generation — the backend never restarted, so
    // the renderer's live state is still valid and should not be discarded.
    expect(recovered.state.generation).toBe(1)
  })
})

describe('shutdown', () => {
  it('a deliberate stop is not treated as a crash', () => {
    const ready = drive([{ type: 'start' }, { type: 'spawned', pid: 1 }, goodHandshake])
    const stopped = drive(
      [
        { type: 'stop-requested' },
        { type: 'process-exit', code: 0, signal: 'SIGTERM', stderrTail: '' },
      ],
      ready.state,
    )
    expect(stopped.state.phase).toBe('stopped')
    expect(stopped.effects.some((e) => e.type === 'schedule-backoff')).toBe(false)
  })

  it('asks for a graceful kill before a hard one', () => {
    const ready = drive([{ type: 'start' }, { type: 'spawned', pid: 1 }, goodHandshake])
    const { effects } = drive([{ type: 'stop-requested' }], ready.state)
    const kill = effects.find((e) => e.type === 'kill')
    expect(kill).toBeDefined()
    expect(kill?.type === 'kill' && kill.graceMs).toBeGreaterThan(0)
  })
})
