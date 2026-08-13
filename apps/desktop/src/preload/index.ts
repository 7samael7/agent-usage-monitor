/**
 * The preload bridge.
 *
 * Exposes exactly one frozen object with named methods. No `ipcRenderer` leak,
 * no dynamic channel names, no generic `invoke(channel, ...)` passthrough — any
 * of those would hand the renderer the whole main-process surface.
 */

import { contextBridge, ipcRenderer } from 'electron'

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

const api = {
  backend: {
    info: (): Promise<BackendInfo> => ipcRenderer.invoke('backend:info'),
    restart: (): Promise<BackendInfo> => ipcRenderer.invoke('backend:restart'),
    logs: (): Promise<string[]> => ipcRenderer.invoke('backend:logs'),
    /** Tell main the event stream is alive, so it can skip the synthetic probe. */
    noteStreamActivity: (): void => ipcRenderer.send('backend:stream-activity'),
    onStateChange: (listener: (info: BackendInfo) => void): (() => void) => {
      const handler = (_e: Electron.IpcRendererEvent, info: BackendInfo) => listener(info)
      ipcRenderer.on('backend:state', handler)
      return () => ipcRenderer.off('backend:state', handler)
    },
  },
  app: {
    info: (): Promise<AppInfo> => ipcRenderer.invoke('app:info'),
  },
  native: {
    pickDirectory: (): Promise<string | null> => ipcRenderer.invoke('native:pickDirectory'),
    pickSavePath: (args: {
      defaultName: string
      filters?: { name: string; extensions: string[] }[]
    }): Promise<string | null> => ipcRenderer.invoke('native:pickSavePath', args),
    reveal: (path: string): Promise<boolean> => ipcRenderer.invoke('native:reveal', { path }),
    /** Only succeeds for a path the user just chose in a save dialog. */
    writeTextFile: (path: string, contents: string): Promise<boolean> =>
      ipcRenderer.invoke('native:writeTextFile', { path, contents }),
  },
} as const

export type MonitorApi = typeof api

contextBridge.exposeInMainWorld('monitor', Object.freeze(api))
