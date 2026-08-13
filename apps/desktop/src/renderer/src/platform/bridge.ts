/**
 * The host bridge.
 *
 * In Electron this is `window.monitor`, supplied by the preload script. In the
 * browser-only development mode (`bun run dev:web`) there is no preload, so a
 * stub reads the connection from environment variables instead.
 *
 * That fallback is not a convenience: running the whole UI in an ordinary
 * browser against a manually started sidecar is the standing proof that no
 * business logic leaked into Electron, and that the backend boundary really is
 * just HTTP.
 */

export interface BackendInfo {
  phase:
    | 'idle'
    | 'spawning'
    | 'handshaking'
    | 'ready'
    | 'degraded'
    | 'restarting'
    | 'failed'
    | 'version-mismatch'
    | 'stopping'
    | 'stopped'
  generation: number
  detail: string | null
  baseUrl: string | null
  token: string | null
  contractVersion: string | null
}

export interface AppInfo {
  version: string
  platform: string
  arch: string
  isPackaged: boolean
  userDataPath: string
  blockedNetworkRequests: number
}

interface Bridge {
  backend: {
    info(): Promise<BackendInfo>
    restart(): Promise<BackendInfo>
    logs(): Promise<string[]>
    noteStreamActivity(): void
    onStateChange(listener: (info: BackendInfo) => void): () => void
  }
  app: { info(): Promise<AppInfo> }
  native: {
    pickDirectory(): Promise<string | null>
    pickSavePath(args: {
      defaultName: string
      filters?: { name: string; extensions: string[] }[]
    }): Promise<string | null>
    reveal(path: string): Promise<boolean>
  }
  /** True when running outside Electron, so the UI can say so plainly. */
  isBrowserFallback: boolean
}

declare global {
  interface Window {
    monitor?: Omit<Bridge, 'isBrowserFallback'>
  }
}

function browserFallback(): Bridge {
  const baseUrl = import.meta.env.VITE_AUM_BASE_URL ?? null
  const token = import.meta.env.VITE_AUM_TOKEN ?? null

  const info: BackendInfo =
    baseUrl && token
      ? {
          phase: 'ready',
          generation: 1,
          detail: null,
          baseUrl,
          token,
          contractVersion: null,
        }
      : {
          phase: 'failed',
          generation: 0,
          detail:
            'Running in the browser without a backend. Start the sidecar and set ' +
            'VITE_AUM_BASE_URL and VITE_AUM_TOKEN, or run the app through Electron.',
          baseUrl: null,
          token: null,
          contractVersion: null,
        }

  return {
    backend: {
      info: () => Promise.resolve(info),
      restart: () => Promise.resolve(info),
      logs: () => Promise.resolve([]),
      noteStreamActivity: () => {},
      onStateChange: () => () => {},
    },
    app: {
      info: () =>
        Promise.resolve({
          version: 'dev',
          platform: 'browser',
          arch: 'unknown',
          isPackaged: false,
          userDataPath: '',
          blockedNetworkRequests: 0,
        }),
    },
    native: {
      pickDirectory: () => Promise.resolve(null),
      pickSavePath: () => Promise.resolve(null),
      reveal: () => Promise.resolve(false),
    },
    isBrowserFallback: true,
  }
}

export const bridge: Bridge = window.monitor
  ? { ...window.monitor, isBrowserFallback: false }
  : browserFallback()
