import { useConnection } from '../backend/backend-provider'
import { useBackend } from '../backend/backend-provider'
import { useEventStream } from '../backend/use-live'
import { Dashboard } from './dashboard'
import { ROUTES, navigate, useLocation } from './router'
import { Applications } from './routes/applications'
import { Benchmarks } from './routes/benchmarks'
import { History } from './routes/history'
import { LiveTasks } from './routes/live-tasks'
import { Models } from './routes/models'
import { Settings } from './routes/settings'

export function AppShell() {
  const { info } = useBackend()
  const conn = useConnection()
  const location = useLocation()

  // One stream for the whole application. Every screen reads the same store, so
  // navigating between them does not reconnect.
  useEventStream(conn)

  return (
    <div className="flex h-full flex-col bg-bg text-text">
      <TitleBar />

      {info.phase === 'restarting' && (
        <Banner>
          The backend is restarting. Live data is paused and the numbers below are stale.
        </Banner>
      )}
      {info.phase === 'degraded' && (
        <Banner>
          The backend is not answering health checks. It may be busy; collection is continuing.
        </Banner>
      )}

      <div className="flex min-h-0 flex-1">
        <Sidebar current={location.path} />
        <main className="min-w-0 flex-1 overflow-auto">
          <Route path={location.path} />
        </main>
      </div>

      <StatusBar />
    </div>
  )
}

function Route({ path }: { path: string }) {
  switch (path) {
    case '/live':
      return <LiveTasks />
    case '/benchmarks':
      return <Benchmarks />
    case '/history':
      return <History />
    case '/models':
      return <Models />
    case '/applications':
      return <Applications />
    case '/settings':
      return <Settings />
    default:
      return <Dashboard />
  }
}

function TitleBar() {
  return (
    <header className="drag-region flex h-11 shrink-0 items-center border-border border-b bg-surface pr-3 pl-20">
      <span className="font-medium text-[13px] text-text-dim">Agent Usage Monitor</span>
    </header>
  )
}

function Sidebar({ current }: { current: string }) {
  return (
    <nav className="w-52 shrink-0 border-border border-r bg-surface p-2">
      <ul className="flex flex-col gap-0.5">
        {ROUTES.map((route) => {
          const active = current === route.path
          return (
            <li key={route.path}>
              <button
                type="button"
                onClick={() => navigate(route.path)}
                className={`no-drag w-full rounded px-2.5 py-1.5 text-left transition-colors ${
                  active ? 'bg-surface-3 text-text' : 'text-text-dim hover:bg-surface-2'
                }`}
              >
                {route.label}
              </button>
            </li>
          )
        })}
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

function Banner({ children }: { children: React.ReactNode }) {
  return (
    <div className="shrink-0 border-warn/40 border-b bg-warn/10 px-4 py-2 text-[12px] text-warn">
      {children}
    </div>
  )
}
