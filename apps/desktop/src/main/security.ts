/**
 * Privacy, enforced rather than promised.
 *
 * The application claims to be local-first and to never upload captured data.
 * Making that structurally true is cheap, so we do it: the renderer is given no
 * route to the network at all except the local sidecar. Any outbound request
 * the app legitimately makes (exchange rates, pricing updates) is issued by the
 * sidecar, behind individually disableable settings.
 */

import { type Session, app, shell } from 'electron'

export interface NetworkGuard {
  /** Requests cancelled because they were not the local sidecar. */
  readonly blockedCount: () => number
  readonly blockedSamples: () => string[]
  readonly setSidecarOrigin: (origin: string | null) => void
}

const MAX_SAMPLES = 20

export function installNetworkGuard(session: Session, appScheme: string): NetworkGuard {
  let blocked = 0
  const samples: string[] = []
  let sidecarOrigin: string | null = null

  const isAllowed = (url: string): boolean => {
    if (url.startsWith(`${appScheme}://`)) return true
    if (url.startsWith('devtools://') || url.startsWith('blob:') || url.startsWith('data:')) {
      return true
    }
    // Vite's dev server and HMR websocket.
    if (!app.isPackaged && /^(http|ws)s?:\/\/(localhost|127\.0\.0\.1):\d+/.test(url)) return true
    if (sidecarOrigin && url.startsWith(sidecarOrigin)) return true
    return false
  }

  session.webRequest.onBeforeRequest((details, callback) => {
    if (isAllowed(details.url)) {
      callback({ cancel: false })
      return
    }
    blocked += 1
    if (samples.length < MAX_SAMPLES) samples.push(details.url.slice(0, 200))
    callback({ cancel: true })
  })

  session.webRequest.onHeadersReceived((details, callback) => {
    const connectSrc = ["'self'", 'http://127.0.0.1:*']
    if (!app.isPackaged) connectSrc.push('ws://localhost:*', 'http://localhost:*')

    // In development Vite injects React Fast Refresh as an *inline* module
    // script, which a strict `script-src 'self'` blocks — presenting as the
    // opaque "@vitejs/plugin-react can't detect preamble". Relaxed for the dev
    // server only; the packaged app keeps `script-src 'self'`, which is the
    // policy that actually matters.
    const scriptSrc = app.isPackaged ? "script-src 'self'" : "script-src 'self' 'unsafe-inline'"

    callback({
      responseHeaders: {
        ...details.responseHeaders,
        'Content-Security-Policy': [
          [
            "default-src 'self'",
            `connect-src ${connectSrc.join(' ')}`,
            "img-src 'self' data:",
            // Vite injects styles inline in development; Tailwind emits a single
            // stylesheet in production, but the inline allowance is kept so the
            // two builds do not diverge in a way only production would reveal.
            "style-src 'self' 'unsafe-inline'",
            scriptSrc,
            "font-src 'self' data:",
            "object-src 'none'",
            "frame-src 'none'",
            "base-uri 'none'",
            "form-action 'none'",
          ].join('; '),
        ],
      },
    })
  })

  // No permissions are needed by this application. Denying by default means a
  // future dependency cannot quietly acquire one.
  session.setPermissionRequestHandler((_wc, _permission, callback) => callback(false))
  session.setPermissionCheckHandler(() => false)

  return {
    blockedCount: () => blocked,
    blockedSamples: () => [...samples],
    setSidecarOrigin: (origin) => {
      sidecarOrigin = origin
    },
  }
}

/**
 * Navigation and window-opening lockdown.
 *
 * A monitoring tool has no reason to navigate anywhere or open a second window.
 * External links go to the user's browser, where they belong.
 */
export function lockDownNavigation(
  contents: Electron.WebContents,
  allowedPrefixes: string[],
): void {
  contents.setWindowOpenHandler(({ url }) => {
    if (/^https?:\/\//.test(url)) void shell.openExternal(url)
    return { action: 'deny' }
  })

  contents.on('will-navigate', (event, url) => {
    if (!allowedPrefixes.some((p) => url.startsWith(p))) {
      event.preventDefault()
      if (/^https?:\/\//.test(url)) void shell.openExternal(url)
    }
  })

  contents.on('will-attach-webview', (event) => event.preventDefault())
}
