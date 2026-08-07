import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// The bundle is embedded into the `decman-wallet-demo` binary (rust-embed reads
// ./dist), so everything must be self-contained — no CDN fonts, no runtime
// fetches to anywhere but this wallet's own API.
//
// `npm run dev` proxies /api to a demo wallet already running on :7878, so the UI
// can be iterated on with hot reload against a live hosting set.
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      '/api': 'http://127.0.0.1:7878',
    },
  },
})
