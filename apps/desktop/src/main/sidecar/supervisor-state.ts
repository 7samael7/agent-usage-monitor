/**
 * The sidecar supervisor, as a pure function.
 *
 * `next(state, event) -> [state, effects]` performs no I/O and imports nothing
 * from Electron or `node:child_process`. That is the only way the restart,
 * backoff and failure logic gets real unit tests — the impure shell in
 * `supervisor.ts` just carries out the effects this returns.
 */

export type BackendPhase =
  | 'idle'
  | 'spawning'
  | 'handshaking'
  | 'ready'
  /** Health checks are failing but the process is alive. No restart yet. */
  | 'degraded'
  | 'restarting'
  /** Gave up. Manual retry only — never loop-restart a broken binary. */
  | 'failed'
  /** Contract mismatch. A hard stop, deliberately not recoverable. */
  | 'version-mismatch'
  | 'stopping'
  | 'stopped'

export interface Connection {
  readonly baseUrl: string
  readonly token: string
  readonly pid: number
  readonly contractVersion: string
}

export interface SupervisorState {
  readonly phase: BackendPhase
  /** Incremented on every successful start. A change means "throw away all live state". */
  readonly generation: number
  readonly connection: Connection | null
  /** Consecutive failed starts. Reset after a sustained healthy period. */
  readonly attempt: number
  /** Consecutive failed health checks. */
  readonly missedHealthChecks: number
  /** Human-readable cause, shown on the failure screen. */
  readonly detail: string | null
  /** Timestamp of the last transition into `ready`, for the attempt reset. */
  readonly readySince: number | null
}

export type SupervisorEvent =
  | { type: 'start' }
  | { type: 'spawned'; pid: number }
  | { type: 'handshake'; port: number; pid: number; contractVersion: string; token: string }
  | { type: 'handshake-timeout' }
  | { type: 'handshake-malformed'; detail: string }
  | { type: 'spawn-error'; detail: string }
  | { type: 'process-exit'; code: number | null; signal: string | null; stderrTail: string }
  | { type: 'health-ok' }
  | { type: 'health-fail'; detail: string }
  | { type: 'backoff-elapsed' }
  | { type: 'retry-requested' }
  | { type: 'stop-requested' }
  | { type: 'stopped' }
  | { type: 'tick'; now: number }

export type Effect =
  | { type: 'spawn' }
  | { type: 'kill'; graceMs: number }
  | { type: 'schedule-backoff'; delayMs: number }
  | { type: 'start-health-checks' }
  | { type: 'stop-health-checks' }
  | { type: 'notify-renderer' }

/** The version of the contract this build of the desktop app was compiled against. */
export const EXPECTED_CONTRACT_MAJOR = 1

/** 500ms, 1s, 2s, 4s, 8s, 16s, then capped at 30s. */
export const BACKOFF_MS = [500, 1_000, 2_000, 4_000, 8_000, 16_000, 30_000] as const
/** Give up after this many consecutive failed starts. */
export const MAX_ATTEMPTS = 5
/** Consecutive failed health checks before declaring `degraded`. */
export const HEALTH_FAILS_BEFORE_DEGRADED = 3
/** Stay `ready` this long and the attempt counter resets. */
export const STABLE_PERIOD_MS = 60_000

export const initialState: SupervisorState = {
  phase: 'idle',
  generation: 0,
  connection: null,
  attempt: 0,
  missedHealthChecks: 0,
  detail: null,
  readySince: null,
}

export function backoffFor(attempt: number): number {
  const idx = Math.min(attempt, BACKOFF_MS.length - 1)
  return BACKOFF_MS[idx] ?? 30_000
}

function majorOf(version: string): number {
  return Number.parseInt(version.split('.')[0] ?? '', 10)
}

/**
 * Decide what happens next.
 *
 * The rule that matters most here: a contract-version mismatch is terminal. It
 * would be easy to "degrade gracefully" and carry on, and that is exactly how a
 * UI starts rendering confident wrong numbers against a backend that means
 * something different by the same field names.
 */
export function next(state: SupervisorState, event: SupervisorEvent): [SupervisorState, Effect[]] {
  switch (event.type) {
    case 'start':
      if (state.phase === 'spawning' || state.phase === 'handshaking' || state.phase === 'ready') {
        return [state, []]
      }
      return [
        { ...state, phase: 'spawning', detail: null, missedHealthChecks: 0 },
        [{ type: 'spawn' }, { type: 'notify-renderer' }],
      ]

    case 'spawned':
      return [{ ...state, phase: 'handshaking' }, []]

    case 'handshake': {
      const major = majorOf(event.contractVersion)
      if (major !== EXPECTED_CONTRACT_MAJOR) {
        return [
          {
            ...state,
            phase: 'version-mismatch',
            connection: null,
            detail: `The backend speaks contract ${event.contractVersion}, but this build expects ${EXPECTED_CONTRACT_MAJOR}.x. Refusing to continue: reading its data with mismatched types could show numbers that are quietly wrong.`,
          },
          [{ type: 'kill', graceMs: 3_000 }, { type: 'notify-renderer' }],
        ]
      }
      return [
        {
          ...state,
          phase: 'ready',
          generation: state.generation + 1,
          connection: {
            baseUrl: `http://127.0.0.1:${event.port}`,
            token: event.token,
            pid: event.pid,
            contractVersion: event.contractVersion,
          },
          missedHealthChecks: 0,
          detail: null,
          readySince: Date.now(),
        },
        [{ type: 'start-health-checks' }, { type: 'notify-renderer' }],
      ]
    }

    case 'handshake-timeout':
      return fail(state, 'The backend started but never announced itself.', {
        killFirst: true,
      })

    case 'handshake-malformed':
      return fail(state, `The backend sent an unreadable handshake: ${event.detail}`, {
        killFirst: true,
      })

    case 'spawn-error':
      return fail(state, event.detail, { killFirst: false })

    case 'process-exit': {
      if (state.phase === 'stopping' || state.phase === 'stopped') {
        return [{ ...state, phase: 'stopped', connection: null }, [{ type: 'notify-renderer' }]]
      }
      if (state.phase === 'version-mismatch') {
        return [{ ...state, connection: null }, []]
      }
      const how = event.signal ? `signal ${event.signal}` : `exit code ${event.code}`
      const detail = event.stderrTail
        ? `The backend exited (${how}).\n\n${event.stderrTail}`
        : `The backend exited (${how}).`
      return fail(state, detail, { killFirst: false })
    }

    case 'health-ok':
      if (state.phase === 'degraded') {
        return [
          { ...state, phase: 'ready', missedHealthChecks: 0, detail: null },
          [{ type: 'notify-renderer' }],
        ]
      }
      return [{ ...state, missedHealthChecks: 0 }, []]

    case 'health-fail': {
      const missed = state.missedHealthChecks + 1
      if (missed >= HEALTH_FAILS_BEFORE_DEGRADED && state.phase === 'ready') {
        // Degraded, not restarting: the sidecar may simply be busy on a slow
        // write. Killing it here would turn a hiccup into lost data.
        return [
          {
            ...state,
            phase: 'degraded',
            missedHealthChecks: missed,
            detail: `The backend has not answered ${missed} health checks. ${event.detail}`,
          },
          [{ type: 'notify-renderer' }],
        ]
      }
      return [{ ...state, missedHealthChecks: missed }, []]
    }

    case 'backoff-elapsed':
      if (state.phase !== 'restarting') return [state, []]
      return [{ ...state, phase: 'spawning' }, [{ type: 'spawn' }, { type: 'notify-renderer' }]]

    case 'retry-requested':
      // A manual retry clears the attempt budget — the user has looked at the
      // error and decided to try again.
      return [
        { ...state, phase: 'spawning', attempt: 0, detail: null },
        [{ type: 'spawn' }, { type: 'notify-renderer' }],
      ]

    case 'stop-requested':
      return [
        { ...state, phase: 'stopping' },
        [
          { type: 'stop-health-checks' },
          { type: 'kill', graceMs: 3_000 },
          { type: 'notify-renderer' },
        ],
      ]

    case 'stopped':
      return [{ ...state, phase: 'stopped', connection: null }, [{ type: 'notify-renderer' }]]

    case 'tick': {
      if (state.phase === 'ready' && state.readySince !== null && state.attempt > 0) {
        if (event.now - state.readySince >= STABLE_PERIOD_MS) {
          return [{ ...state, attempt: 0 }, []]
        }
      }
      return [state, []]
    }

    default: {
      // Exhaustiveness: adding a variant to SupervisorEvent without handling it
      // here is a compile error, not a silently ignored event.
      const unhandled: never = event
      void unhandled
      return [state, []]
    }
  }
}

function fail(
  state: SupervisorState,
  detail: string,
  opts: { killFirst: boolean },
): [SupervisorState, Effect[]] {
  const attempt = state.attempt + 1
  const effects: Effect[] = [{ type: 'stop-health-checks' }]
  if (opts.killFirst) effects.push({ type: 'kill', graceMs: 1_000 })

  if (attempt >= MAX_ATTEMPTS) {
    return [
      {
        ...state,
        phase: 'failed',
        connection: null,
        attempt,
        detail: `${detail}\n\nGiving up after ${attempt} attempts.`,
        readySince: null,
      },
      [...effects, { type: 'notify-renderer' }],
    ]
  }

  return [
    { ...state, phase: 'restarting', connection: null, attempt, detail, readySince: null },
    [
      ...effects,
      { type: 'schedule-backoff', delayMs: backoffFor(attempt - 1) },
      { type: 'notify-renderer' },
    ],
  ]
}
