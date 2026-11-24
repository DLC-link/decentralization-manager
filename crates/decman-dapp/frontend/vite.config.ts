import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// The Splice localnet publishes each participant's JSON Ledger API on port
// 975 behind a prefix: 3 for the app provider, 2 for the app user, 4 for the
// SV (docker-compose/localnet/env/common.env in the localnet bundle). The
// proxy keeps the browser on one origin, so the participants need no CORS.
const LOCALNET_JSON_API_PORTS: Record<string, number> = {
  'app-provider': 3975,
  'app-user': 2975,
  sv: 4975,
}

const proxy = Object.fromEntries(
  Object.entries(LOCALNET_JSON_API_PORTS).map(([name, port]) => {
    const prefix = `/participants/${name}`
    return [
      prefix,
      {
        target: `http://localhost:${port}`,
        changeOrigin: true,
        rewrite: (path: string) => path.slice(prefix.length),
      },
    ]
  }),
)

// The WebAssembly module loads through `new URL(..., import.meta.url)` in the
// wasm-pack output, which Vite serves and bundles without a plugin.
export default defineConfig({
  plugins: [react()],
  server: { proxy },
})
