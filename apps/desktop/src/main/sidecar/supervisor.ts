/**
 * The impure shell around the pure state machine.
 *
 * Everything decidable lives in `supervisor-state.ts`; this file only spawns
 * processes, reads pipes, sets timers, and forwards state to the renderer.
 */

import { type ChildProcess, spawn } from 'node:child_process'
import crypto from 'node:crypto'
import { app } from 'electron'
import { resolveSidecarBinary } from './resolve-binary'
import {
  type Connection,
  type Effect,
  type SupervisorEvent,
  type SupervisorState,
  initialState,
  next,
} from './supervisor-state'

/** How long to wait for the handshake line before declaring the start failed. */
const HANDSHAKE_TIMEOUT_MS = 10_000
const HEALTH_INTERVAL_MS = 5_000
const HEALTH_TIMEOUT_MS = 2_000
/** Keep this much stderr for the failure screen. */
const STDERR_TAIL_LINES = 200

export interface BackendInfo {
  phase: SupervisorState['phase']
  generation: number
  detail: string | null
  baseUrl: string | null
  token: string | null
  contractVersion: string | null
}

export class SidecarSupervisor {
  #state: SupervisorState = initialState
  #child: ChildProcess | null = null
  #token: string | null = null
  #stderr: string[] = []
  #stdoutBuffer = ''
  #handshakeSeen = false
  #handshakeTimer: NodeJS.Timeout | null = null
  #backoffTimer: NodeJS.Timeout | null = null
  #healthTimer: NodeJS.Timeout | null = null
  #tickTimer: NodeJS.Timeout | null = null
  /** Set when an SSE event arrived recently; suppresses the synthetic probe. */
  #lastStreamActivity = 0

  constructor(
    private readonly allowedOrigin: string,
    private readonly onChange: (info: BackendInfo) => void,
  ) {
    this.#tickTimer = setInterval(() => this.#send({ type: 'tick', now: Date.now() }), 10_000)
    this.#tickTimer.unref?.()
  }

  get info(): BackendInfo {
    const c: Connection | null = this.#state.connection
    return {
      phase: this.#state.phase,
      generation: this.#state.generation,
      detail: this.#state.detail,
      baseUrl: c?.baseUrl ?? null,
      token: c?.token ?? null,
      contractVersion: c?.contractVersion ?? null,
    }
  }

  get stderrTail(): string[] {
    return [...this.#stderr]
  }

  start(): void {
    this.#send({ type: 'start' })
  }

  retry(): void {
    this.#send({ type: 'retry-requested' })
  }

  /** Called when an SSE frame arrives, so health probing can stand down. */
  noteStreamActivity(): void {
    this.#lastStreamActivity = Date.now()
  }

  async stop(): Promise<void> {
    this.#send({ type: 'stop-requested' })
    const child = this.#child
    if (!child || child.exitCode !== null) return

    await new Promise<void>((resolve) => {
      const hard = setTimeout(() => {
        child.kill('SIGKILL')
        resolve()
      }, 3_000)
      child.once('exit', () => {
        clearTimeout(hard)
        resolve()
      })
    })
  }

  dispose(): void {
    for (const t of [
      this.#handshakeTimer,
      this.#backoffTimer,
      this.#healthTimer,
      this.#tickTimer,
    ]) {
      if (t) clearTimeout(t)
    }
  }

  // ── the loop ───────────────────────────────────────────────────────────────

  #send(event: SupervisorEvent): void {
    const [state, effects] = next(this.#state, event)
    this.#state = state
    for (const effect of effects) this.#run(effect)
  }

  #run(effect: Effect): void {
    switch (effect.type) {
      case 'spawn':
        this.#spawn()
        break
      case 'kill':
        this.#child?.kill('SIGTERM')
        break
      case 'schedule-backoff':
        if (this.#backoffTimer) clearTimeout(this.#backoffTimer)
        this.#backoffTimer = setTimeout(
          () => this.#send({ type: 'backoff-elapsed' }),
          effect.delayMs,
        )
        break
      case 'start-health-checks':
        this.#startHealthChecks()
        break
      case 'stop-health-checks':
        if (this.#healthTimer) clearInterval(this.#healthTimer)
        this.#healthTimer = null
        break
      case 'notify-renderer':
        this.onChange(this.info)
        break
    }
  }

  #spawn(): void {
    const binary = resolveSidecarBinary()
    if (!binary.exists) {
      this.#send({
        type: 'spawn-error',
        detail: `Backend binary not found at ${binary.path}.\n\n${binary.hint}`,
      })
      return
    }

    // 256 bits, regenerated every start. Passed in the environment, never in
    // argv — argv is world-readable via `ps aux`.
    this.#token = crypto.randomBytes(32).toString('base64url')
    this.#stderr = []
    this.#stdoutBuffer = ''
    this.#handshakeSeen = false

    let child: ChildProcess
    try {
      child = spawn(binary.path, [], {
        stdio: ['pipe', 'pipe', 'pipe'],
        detached: false,
        env: {
          ...process.env,
          AUM_TOKEN: this.#token,
          AUM_PARENT_PID: String(process.pid),
          AUM_DATA_DIR: app.getPath('userData'),
          AUM_ALLOWED_ORIGIN: this.allowedOrigin,
        },
      })
    } catch (e) {
      this.#send({ type: 'spawn-error', detail: `Could not start the backend: ${String(e)}` })
      return
    }

    this.#child = child
    child.stdout?.setEncoding('utf8')
    child.stderr?.setEncoding('utf8')
    child.stdout?.on('data', (chunk: string) => this.#onStdout(chunk))
    child.stderr?.on('data', (chunk: string) => this.#onStderr(chunk))

    child.on('error', (e) => {
      this.#send({ type: 'spawn-error', detail: `Could not start the backend: ${e.message}` })
    })

    child.on('exit', (code, signal) => {
      if (this.#handshakeTimer) clearTimeout(this.#handshakeTimer)
      this.#child = null
      this.#send({
        type: 'process-exit',
        code,
        signal,
        stderrTail: this.#stderr.slice(-40).join('\n'),
      })
    })

    this.#handshakeTimer = setTimeout(() => {
      if (!this.#handshakeSeen) this.#send({ type: 'handshake-timeout' })
    }, HANDSHAKE_TIMEOUT_MS)

    this.#send({ type: 'spawned', pid: child.pid ?? -1 })
  }

  /**
   * The backend writes exactly one line to stdout and then stops using it, so we
   * only ever parse the first line and ignore any later noise rather than
   * misinterpreting it.
   */
  #onStdout(chunk: string): void {
    if (this.#handshakeSeen) return
    this.#stdoutBuffer += chunk
    const newline = this.#stdoutBuffer.indexOf('\n')
    if (newline === -1) return

    const line = this.#stdoutBuffer.slice(0, newline).trim()
    this.#handshakeSeen = true
    if (this.#handshakeTimer) clearTimeout(this.#handshakeTimer)

    let parsed: unknown
    try {
      parsed = JSON.parse(line)
    } catch {
      this.#send({
        type: 'handshake-malformed',
        detail: `expected JSON, got ${JSON.stringify(line.slice(0, 200))}`,
      })
      return
    }

    const h = parsed as Record<string, unknown>
    if (h.kind !== 'aum.handshake') {
      this.#send({ type: 'handshake-malformed', detail: `unexpected kind ${String(h.kind)}` })
      return
    }
    const port = typeof h.port === 'number' ? h.port : Number.NaN
    const contractVersion = typeof h.contract_version === 'string' ? h.contract_version : ''
    if (!Number.isInteger(port) || port <= 0 || !contractVersion) {
      this.#send({ type: 'handshake-malformed', detail: 'missing port or contract_version' })
      return
    }

    this.#send({
      type: 'handshake',
      port,
      pid: typeof h.pid === 'number' ? h.pid : -1,
      contractVersion,
      token: this.#token ?? '',
    })
  }

  #onStderr(chunk: string): void {
    for (const line of chunk.split('\n')) {
      if (!line.trim()) continue
      this.#stderr.push(line)
      // In development, surface the backend's logs in the terminal. Without
      // this they are invisible unless the process fails outright, which makes
      // "the request was refused and I cannot see why" needlessly hard.
      if (!app.isPackaged) process.stderr.write(`[sidecar] ${line}\n`)
    }
    if (this.#stderr.length > STDERR_TAIL_LINES) {
      this.#stderr = this.#stderr.slice(-STDERR_TAIL_LINES)
    }
  }

  #startHealthChecks(): void {
    if (this.#healthTimer) clearInterval(this.#healthTimer)
    this.#healthTimer = setInterval(() => void this.#probe(), HEALTH_INTERVAL_MS)
    this.#healthTimer.unref?.()
  }

  async #probe(): Promise<void> {
    // A live event stream is better evidence of health than a synthetic probe,
    // and skipping the probe avoids the classic "healthy endpoint, dead stream"
    // blind spot working the other way round.
    if (Date.now() - this.#lastStreamActivity < HEALTH_INTERVAL_MS) {
      this.#send({ type: 'health-ok' })
      return
    }

    const base = this.#state.connection?.baseUrl
    if (!base) return

    try {
      const controller = new AbortController()
      const timer = setTimeout(() => controller.abort(), HEALTH_TIMEOUT_MS)
      const res = await fetch(`${base}/v1/health`, { signal: controller.signal })
      clearTimeout(timer)
      if (res.ok) this.#send({ type: 'health-ok' })
      else this.#send({ type: 'health-fail', detail: `HTTP ${res.status}` })
    } catch (e) {
      this.#send({ type: 'health-fail', detail: String(e) })
    }
  }
}
