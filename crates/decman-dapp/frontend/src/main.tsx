import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'

import { App } from './App'
import { loadChecks } from './checks'
import './style.css'

const root = createRoot(document.getElementById('root')!)

// The pages call the Rust checks while they render, so the WebAssembly module
// loads first.
loadChecks().then(
  () =>
    root.render(
      <StrictMode>
        <App />
      </StrictMode>,
    ),
  (error: unknown) => root.render(<p className="error">Could not load the WebAssembly checks: {String(error)}</p>),
)
