import type { ReactNode } from 'react'
import { useBackend } from '../backend/backend-provider'

const NAV = [
  'Dashboard',
  'Live Tasks',
  'Benchmarks',
  'History',
  'Models & Pricing',
  'Applications',
  'Settings',
] as const

export function AppShell({ children }: { children: ReactNode }) {
  const { info } = useBackend()

  return (
    <div className="flex h-full flex-col bg-bg text-text">
      <TitleBar />
      {info.phase === 'restarting' && (
        <Banner tone="warn">
          The backend is restarting. Live data is paused, and the numbers below are stale.
        </Banner>
      )}
      {info.phase === 'degraded' && (
        <Banner tone="warn">
          The backend is not responding to health checks. It may be busy; data collection is
          continuing.
        </Banner>
      )}
      <div className="flex min-h-0 flex-1">
        <Sidebar />
        <main className="min-w-0 flex-1 overflow-auto">{children}</main>
      </div>
      <StatusBar />
    </div>
  )
}

function TitleBar() {
  return (
    <header className="drag-region flex h-11 shrink-0 items-center border-border border-b bg-surface pr-3 pl-20">
      <span className="font-medium text-[13px] text-text-dim">Agent Usage Monitor</span>
    </header>
  )
}

function Sidebar() {
  return (
    <nav className="w-52 shrink-0 border-border border-r bg-surface p-2">
      <ul className="flex flex-col gap-0.5">
        {NAV.map((item, i) => (
          <li key={item}>
            <button
              type="button"
              disabled={i !== 0}
              className={
                i === 0
                  ? 'w-full rounded bg-surface-3 px-2.5 py-1.5 text-left text-text'
                  : 'w-full cursor-not-allowed rounded px-2.5 py-1.5 text-left text-text-faint'
              }
            >
              {item}
            </button>
          </li>
        ))}
      </ul>
    </nav>
  )
}

function StatusBar() {
  const { info } = useBackend()
  const tone = info.phase === 'ready' ? 'bg-exact' : info.phase === 'degraded' ? 'bg-warn' : 'bg-na'

  return (
    <footer className="flex h-7 shrink-0 items-center gap-3 border-border border-t bg-surface px-3 text-[11px] text-text-mute">
      <span className="flex items-center gap-1.5">
        <span className={`size-1.5 rounded-full ${tone}`} aria-hidden />
        backend {info.phase}
      </span>
      {info.contractVersion && <span>contract {info.contractVersion}</span>}
      <span className="ml-auto">generation {info.generation}</span>
    </footer>
  )
}

function Banner({ tone, children }: { tone: 'warn' | 'neg'; children: ReactNode }) {
  const cls =
    tone === 'warn' ? 'border-warn/40 bg-warn/10 text-warn' : 'border-neg/40 bg-neg/10 text-neg'
  return <div className={`shrink-0 border-b px-4 py-2 text-[12px] ${cls}`}>{children}</div>
}
