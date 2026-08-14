/**
 * A custom `app://` scheme for the renderer.
 *
 * Loading from `file://` would give the renderer the origin `null`, which makes
 * CORS against `http://127.0.0.1:PORT` fragile and forces permissive CORS on
 * the server side. A registered standard scheme gives us one stable, known
 * origin — which is exactly what the sidecar's Origin allowlist and the CSP
 * both need, and what `localStorage` needs to persist.
 */

import { existsSync } from 'node:fs'
import { readFile } from 'node:fs/promises'
import path from 'node:path'
import { protocol } from 'electron'

export const APP_SCHEME = 'app'
export const RENDERER_ORIGIN = `${APP_SCHEME}://local`

/** Must be called before `app.whenReady()`. */
export function registerSchemes(): void {
  protocol.registerSchemesAsPrivileged([
    {
      scheme: APP_SCHEME,
      privileges: {
        standard: true,
        secure: true,
        supportFetchAPI: true,
        corsEnabled: true,
        stream: true,
      },
    },
  ])
}

/**
 * Content types for what a Vite build emits.
 *
 * Required because this handler builds its own `Response`: without an explicit
 * `Content-Type`, Chromium sniffs, and a sniffed `index.html` renders as plain
 * text while a sniffed module script is refused outright.
 */
const CONTENT_TYPES: Record<string, string> = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.webp': 'image/webp',
  '.ico': 'image/x-icon',
  '.woff': 'font/woff',
  '.woff2': 'font/woff2',
  '.ttf': 'font/ttf',
  '.map': 'application/json; charset=utf-8',
}

export function registerAppProtocol(): void {
  const rendererRoot = path.join(import.meta.dirname, '../renderer')

  protocol.handle(APP_SCHEME, async (request) => {
    const url = new URL(request.url)
    const relative = decodeURIComponent(url.pathname).replace(/^\/+/, '')
    const resolved = path.join(rendererRoot, relative || 'index.html')

    // Refuse anything that escapes the renderer directory. Without this, a
    // crafted `app://local/../../..` could read arbitrary files.
    const normalizedRoot = path.resolve(rendererRoot)
    if (!path.resolve(resolved).startsWith(normalizedRoot)) {
      return new Response('Forbidden', { status: 403 })
    }

    // Single-page app: unknown paths fall back to the shell so client-side
    // routing works on a hard reload.
    const target = existsSync(resolved) ? resolved : path.join(rendererRoot, 'index.html')

    // Read the file directly rather than delegating to `net.fetch` on a
    // `file://` URL. That delegation routes back through the session's
    // `onBeforeRequest`, where this application's own network guard cancels
    // it — `file://` is not the sidecar and is not meant to be reachable. The
    // packaged app therefore failed to load its own interface with
    // ERR_BLOCKED_BY_CLIENT, while development, which loads from the Vite dev
    // server, never touched this path at all.
    //
    // Reading here is also simply more direct: the path has already been
    // resolved and checked, so a round trip through the network stack was
    // buying nothing.
    try {
      const body = await readFile(target)
      return new Response(body, {
        status: 200,
        headers: {
          'Content-Type': CONTENT_TYPES[path.extname(target).toLowerCase()] ?? 'text/plain',
          // The bundle is content-hashed by Vite and shipped inside the app, so
          // the only correct cache lifetime is "as long as this build exists".
          'Cache-Control': 'no-cache',
        },
      })
    } catch {
      return new Response('Not found', { status: 404 })
    }
  })
}
