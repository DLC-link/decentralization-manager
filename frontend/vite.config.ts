import { existsSync, readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { defineConfig, type Plugin } from 'vite'
import react from '@vitejs/plugin-react'

const rootDir = dirname(fileURLToPath(import.meta.url))

// The crate version (Cargo.toml lives one level up from the frontend). Used as
// the build version when APP_VERSION isn't passed by build.rs — so `npm run dev`
// and mock mode still show the real version rather than a placeholder.
const cargoVersion = (() => {
  try {
    const toml = readFileSync(join(rootDir, '..', 'Cargo.toml'), 'utf8')
    return toml.match(/^version\s*=\s*"([^"]+)"/m)?.[1] ?? null
  } catch {
    return null
  }
})()

// Dev-only API mock. With `MOCK=true npm run dev`, the backend's REST endpoints
// are served from JSON fixtures under ./mocks instead of a live node + Keycloak,
// so the UI renders with realistic data offline. Only applied in `serve` mode;
// production builds never include it.
//
// Routing: the request path maps to a fixture by replacing `/` with `_` and
// appending `.json`, with a longest-prefix fallback (e.g. `/governance/state`
// → `governance_state.json`, `/party-config/<id>` → `party-config.json`).
// Unmapped GETs return `{}`; unmapped POSTs return a completed-workflow stub.
function mockApi(): Plugin {
  const mocksDir = join(rootDir, 'mocks')
  // First path segment of every backend REST route (see src/api.ts callers).
  const apiPrefixes = new Set([
    'auth-config', 'auth', 'node-config', 'network-config', 'network-info',
    'participants-status', 'decentralized-parties', 'operator-info', 'keys',
    'packages', 'invitations', 'workflows', 'governance', 'holdings',
    'instruments', 'party-config', 'services', 'vaults', 'contracts', 'dars',
    'onboarding', 'kick', 'credential-offers', 'token-standard-contracts',
    'transfer-factories', 'transfer-preapprovals',
  ])
  const load = (name: string): string | null => {
    const file = join(mocksDir, `${name}.json`)
    return existsSync(file) ? readFileSync(file, 'utf8') : null
  }
  return {
    name: 'mock-api',
    apply: 'serve',
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const url = new URL(req.url ?? '/', 'http://localhost')
        const segments = url.pathname.replace(/^\/+|\/+$/g, '').split('/')
        if (segments.length === 0 || !apiPrefixes.has(segments[0])) return next()

        // Party-scoped responses: a `?party_id=<prefix>::<ns>` query lets a
        // fixture vary per party via a `<base>__<prefix>.json` override (e.g.
        // governance state/confirmations differ per party). Falls back to the
        // generic `<base>.json` when no per-party override exists.
        const partyId = url.searchParams.get('party_id')
        const tag = partyId
          ? partyId.split('::')[0].replace(/[^A-Za-z0-9._-]/g, '')
          : null

        let body: string | null = null
        for (let i = segments.length; i >= 1 && body === null; i--) {
          const base = segments.slice(0, i).join('_')
          if (tag) body = load(`${base}__${tag}`)
          if (body === null) body = load(base)
        }
        if (body === null) {
          body = req.method === 'POST' ? '{"status":"completed"}' : '{}'
        }
        res.setHeader('Content-Type', 'application/json')
        res.statusCode = 200
        res.end(body)
      })
    },
  }
}

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), ...(process.env.MOCK === 'true' ? [mockApi()] : [])],
  define: {
    __BUILD_DATE__: JSON.stringify(new Date().toISOString()),
    // Build version: APP_VERSION from build.rs (production) → Cargo.toml
    // version (dev / mock) → "dev" only if Cargo.toml can't be read.
    __APP_VERSION__: JSON.stringify(process.env.APP_VERSION ?? cargoVersion ?? 'dev'),
  },
})
