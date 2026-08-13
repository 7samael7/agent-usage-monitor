import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { AppShell } from './app/app-shell'
import { Dashboard } from './app/dashboard'
import { BackendGate } from './backend/backend-gate'
import { BackendProvider } from './backend/backend-provider'
import './styles/theme.css'

const root = document.getElementById('root')
if (!root) throw new Error('#root is missing from index.html')

createRoot(root).render(
  <StrictMode>
    <BackendProvider>
      <BackendGate>
        <AppShell>
          <Dashboard />
        </AppShell>
      </BackendGate>
    </BackendProvider>
  </StrictMode>,
)
