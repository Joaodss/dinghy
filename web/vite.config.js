import { readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { brotliCompressSync, constants, gzipSync } from 'node:zlib'
import { defineConfig } from 'vite'
import preact from '@preact/preset-vite'

const COMPRESSIBLE = /\.(js|css|html|svg|json)$/
const MIN_SIZE = 1024

/**
 * Writes `.br` and `.gz` copies of every text asset. The server embeds them
 * and picks one per request, so it never compresses anything at runtime.
 */
function precompress() {
  let outDir
  return {
    name: 'dinghy-precompress',
    apply: 'build',
    configResolved(config) {
      outDir = config.build.outDir
    },
    closeBundle() {
      const walk = (dir) =>
        readdirSync(dir).flatMap((name) => {
          const path = join(dir, name)
          return statSync(path).isDirectory() ? walk(path) : [path]
        })
      for (const file of walk(outDir)) {
        if (!COMPRESSIBLE.test(file)) continue
        const data = readFileSync(file)
        if (data.length < MIN_SIZE) continue
        writeFileSync(
          `${file}.br`,
          brotliCompressSync(data, { params: { [constants.BROTLI_PARAM_QUALITY]: 11 } }),
        )
        writeFileSync(`${file}.gz`, gzipSync(data, { level: 9 }))
      }
    },
  }
}

export default defineConfig({
  plugins: [preact(), precompress()],
  server: {
    // `npm run dev` talks to a local `dinghy` on its default port.
    proxy: { '/api': 'http://127.0.0.1:5002' },
  },
  build: {
    target: 'es2022',
    chunkSizeWarningLimit: 800,
  },
})
