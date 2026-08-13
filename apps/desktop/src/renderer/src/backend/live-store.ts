/**
 * Live task state, kept outside React.
 *
 * The naive approach — setting React state on every server-sent event — is the
 * first thing that breaks under load. Every update notifies every consumer of
 * the task list, re-runs the table's sort and filter pipeline, and re-renders
 * rows whose numbers did not change. With twenty concurrent tasks that is
 * hundreds of full re-renders a second, in an application whose entire purpose
 * is to not perturb what it measures.
 *
 * So events land in a plain object and subscribers are notified at most once per
 * animation frame, throttled to a few times a second. A component subscribes to
 * exactly the task it renders, via `useSyncExternalStore`, and re-renders only
 * when that task's snapshot identity changes.
 *
 * The snapshots carry **absolute totals**, never deltas, which is what makes a
 * dropped frame harmless: the next one is complete, so a missed event costs a
 * moment of staleness rather than a permanently wrong running total.
 */

import type { StreamEvent, TaskMetrics, TaskSummary } from '@aum/api-contract'

/** How often subscribers are notified, at most. */
const FLUSH_HZ = 4

export interface LiveTask {
  readonly summary: TaskSummary | null
  readonly metrics: TaskMetrics | null
  /** When this task's numbers last moved, for a staleness indicator. */
  readonly updatedAt: number
}

type Listener = () => void

class LiveStore {
  #tasks = new Map<string, LiveTask>()
  #taskListeners = new Map<string, Set<Listener>>()
  #globalListeners = new Set<Listener>()
  #dirty = new Set<string>()
  #globalDirty = false
  #frame: number | null = null
  #lastFlush = 0
  /** Snapshot of the id list, kept stable so `useSyncExternalStore` is happy. */
  #ids: string[] = []
  #streamEpoch: string | null = null
  #contractViolations = 0

  /** Apply one event. Cheap: no notification happens here. */
  apply(event: StreamEvent): void {
    // A new epoch means the backend restarted. Replaying old state into a new
    // generation is a silent-corruption bug, so everything is discarded.
    if (this.#streamEpoch !== null && event.stream_epoch !== this.#streamEpoch) {
      this.clear()
    }
    this.#streamEpoch = event.stream_epoch

    switch (event.type) {
      case 'task_created':
      case 'task_updated': {
        const id = event.task.id
        const previous = this.#tasks.get(id)
        this.#tasks.set(id, {
          summary: event.task,
          metrics: previous?.metrics ?? null,
          updatedAt: Date.now(),
        })
        this.#markGlobal()
        this.#mark(id)
        break
      }

      case 'metrics_snapshot': {
        const id = event.metrics.task_id
        const previous = this.#tasks.get(id)
        this.#tasks.set(id, {
          summary: previous?.summary ?? null,
          metrics: event.metrics,
          updatedAt: Date.now(),
        })
        this.#mark(id)
        break
      }

      case 'task_stopped': {
        if (event.task_id) this.#mark(event.task_id)
        this.#markGlobal()
        break
      }

      // The stream is only ever a hint; the caller refetches on this.
      case 'resync':
        this.clear()
        break

      default:
        break
    }
  }

  /** A frame the client could not understand. Counted, never applied. */
  noteViolation(): void {
    this.#contractViolations += 1
  }

  get contractViolations(): number {
    return this.#contractViolations
  }

  clear(): void {
    this.#tasks.clear()
    this.#ids = []
    this.#markGlobal()
    for (const id of this.#taskListeners.keys()) this.#mark(id)
  }

  // ── subscription ─────────────────────────────────────────────────────────

  subscribeTask(taskId: string, listener: Listener): () => void {
    let set = this.#taskListeners.get(taskId)
    if (!set) {
      set = new Set()
      this.#taskListeners.set(taskId, set)
    }
    set.add(listener)
    return () => {
      set?.delete(listener)
      if (set?.size === 0) this.#taskListeners.delete(taskId)
    }
  }

  subscribeAll(listener: Listener): () => void {
    this.#globalListeners.add(listener)
    return () => this.#globalListeners.delete(listener)
  }

  getTask(taskId: string): LiveTask | undefined {
    return this.#tasks.get(taskId)
  }

  /**
   * Stable array identity while the set of tasks is unchanged.
   *
   * `useSyncExternalStore` compares snapshots by identity, so returning a fresh
   * array each call would loop for ever.
   */
  getIds(): string[] {
    return this.#ids
  }

  #mark(taskId: string): void {
    this.#dirty.add(taskId)
    this.#schedule()
  }

  #markGlobal(): void {
    this.#globalDirty = true
    this.#ids = [...this.#tasks.keys()]
    this.#schedule()
  }

  #schedule(): void {
    if (this.#frame !== null) return

    const since = Date.now() - this.#lastFlush
    const wait = Math.max(0, 1000 / FLUSH_HZ - since)

    const run = () => {
      this.#frame = null
      this.#lastFlush = Date.now()
      this.#flush()
    }

    // Aligned to a frame so a burst of events produces one render, not one per
    // event. Falls back to a timer where there is no animation frame (tests).
    this.#frame = window.setTimeout(() => {
      if (typeof requestAnimationFrame === 'function') requestAnimationFrame(run)
      else run()
    }, wait)
  }

  #flush(): void {
    const dirty = [...this.#dirty]
    this.#dirty.clear()

    for (const id of dirty) {
      const listeners = this.#taskListeners.get(id)
      if (!listeners) continue
      for (const listener of listeners) listener()
    }

    if (this.#globalDirty) {
      this.#globalDirty = false
      for (const listener of this.#globalListeners) listener()
    }
  }
}

export const liveStore = new LiveStore()
