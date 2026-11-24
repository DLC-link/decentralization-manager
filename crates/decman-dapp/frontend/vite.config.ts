import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// The WebAssembly module loads through `new URL(..., import.meta.url)` in the
// wasm-pack output, which Vite serves and bundles without a plugin.
export default defineConfig({
  plugins: [react()],
})
