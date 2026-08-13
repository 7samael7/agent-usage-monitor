/**
 * Server-sent events, read with `fetch` rather than `EventSource`.
 *
 * `EventSource` cannot set request headers, so using it would force the bearer
 * token into the query string — where it lands in logs, in history, and in any
 * error report. A ~60-line frame parser keeps the secret in an `Authorization`
 * header, which is worth far more than the lines it costs.
 */

export interface SseFrame {
  readonly id: string | null
  readonly event: string
  readonly data: string
}

export interface SseOptions {
  readonly baseUrl: string
  readonly token: string
  readonly signal: AbortSignal
  /** Resume point after a reconnect. */
  readonly lastEventId?: string | null
  readonly onFrame: (frame: SseFrame) => void
  readonly onOpen?: () => void
  readonly onError?: (error: unknown) => void
}

/**
 * Consume the stream until aborted. Resolves when the connection ends; the
 * caller owns reconnection policy.
 */
export async function consumeSse(options: SseOptions): Promise<void> {
  const headers: Record<string, string> = {
    Authorization: `Bearer ${options.token}`,
    Accept: 'text/event-stream',
  }
  if (options.lastEventId) headers['Last-Event-ID'] = options.lastEventId

  const res = await fetch(`${options.baseUrl}/v1/events`, {
    headers,
    signal: options.signal,
  })

  if (!res.ok || !res.body) {
    options.onError?.(new Error(`event stream failed: HTTP ${res.status}`))
    return
  }
  options.onOpen?.()

  const reader = res.body.getReader()
  const decoder = new TextDecoder()
  let buffer = ''

  try {
    while (true) {
      const { done, value } = await reader.read()
      if (done) break
      buffer += decoder.decode(value, { stream: true })

      // Frames are separated by a blank line. Anything after the last separator
      // is a partial frame and must stay in the buffer.
      let sep = buffer.indexOf('\n\n')
      while (sep !== -1) {
        const raw = buffer.slice(0, sep)
        buffer = buffer.slice(sep + 2)
        const frame = parseFrame(raw)
        if (frame) options.onFrame(frame)
        sep = buffer.indexOf('\n\n')
      }
    }
  } catch (error) {
    if (!options.signal.aborted) options.onError?.(error)
  } finally {
    reader.releaseLock()
  }
}

function parseFrame(raw: string): SseFrame | null {
  let id: string | null = null
  let event = 'message'
  const dataLines: string[] = []

  for (const line of raw.split('\n')) {
    // A line starting with ':' is a comment — this is how the keep-alive
    // arrives, and it is meaningful: it proves the socket is not half-open.
    if (line.startsWith(':') || line === '') continue

    const colon = line.indexOf(':')
    const field = colon === -1 ? line : line.slice(0, colon)
    const value = colon === -1 ? '' : line.slice(colon + 1).replace(/^ /, '')

    if (field === 'id') id = value
    else if (field === 'event') event = value
    else if (field === 'data') dataLines.push(value)
  }

  if (dataLines.length === 0) return null
  return { id, event, data: dataLines.join('\n') }
}
