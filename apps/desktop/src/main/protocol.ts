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
import path from 'node:path'
import { pathToFileURL } from 'node:url'
import { net, protocol } from 'electron'

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

export function registerAppProtocol(): void {
  const rendererRoot = path.join(import.meta.dirname, '../renderer')

  protocol.handle(APP_SCHEME, (request) => {
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
    return net.fetch(pathToFileURL(target).toString())
  })
}
