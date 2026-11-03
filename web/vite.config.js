import { defineConfig } from 'vite'
import preact from '@preact/preset-vite'

export default defineConfig({
  plugins: [preact()],
  server: {
    // `npm run dev` talks to a local `dinghy` on its default port.
    proxy: { '/api': 'http://127.0.0.1:5002' },
  },
  build: {
    target: 'es2022',
    chunkSizeWarningLimit: 800,
  },
})
