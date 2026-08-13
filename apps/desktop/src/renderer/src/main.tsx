import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { AppShell } from './app/app-shell'
import { BackendGate } from './backend/backend-gate'
import { BackendProvider } from './backend/backend-provider'
import './styles/theme.css'

const root = document.getElementById('root')
if (!root) throw new Error('#root is missing from index.html')

createRoot(root).render(
  <StrictMode>
    <BackendProvider>
      <BackendGate>
        <AppShell />
      </BackendGate>
    </BackendProvider>
  </StrictMode>,
)
