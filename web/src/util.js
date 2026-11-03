import { useEffect, useState } from 'preact/hooks'

export function formatBytes(bytes) {
  if (bytes == null) return '—'
  const mib = bytes / (1024 * 1024)
  if (mib >= 1024) return `${(mib / 1024).toFixed(2)} GiB`
  if (mib >= 100) return `${mib.toFixed(0)} MiB`
  return `${mib.toFixed(1)} MiB`
}

export function formatUptime(startedAt, now = Date.now()) {
  if (!startedAt) return '—'
  const secs = Math.max(0, Math.floor((now - Date.parse(startedAt)) / 1000))
  const d = Math.floor(secs / 86400)
  const h = Math.floor((secs % 86400) / 3600)
  const m = Math.floor((secs % 3600) / 60)
  if (d) return `${d}d ${h}h`
  if (h) return `${h}h ${m}m`
  if (m) return `${m}m`
  return `${secs}s`
}

/** Current `location.hash` without the leading `#`, split into path and query. */
function readHash() {
  const raw = location.hash.replace(/^#/, '') || '/'
  const [path, query = ''] = raw.split('?')
  return { path, query: new URLSearchParams(query) }
}

export function useRoute() {
  const [route, setRoute] = useState(readHash)
  useEffect(() => {
    const onChange = () => setRoute(readHash())
    addEventListener('hashchange', onChange)
    return () => removeEventListener('hashchange', onChange)
  }, [])
  return route
}

export const go = (path) => {
  location.hash = path
}

/** Re-runs `fn` every `ms` while the tab is visible. */
export function useVisibleInterval(fn, ms) {
  useEffect(() => {
    const tick = () => document.visibilityState === 'visible' && fn()
    const id = setInterval(tick, ms)
    document.addEventListener('visibilitychange', tick)
    return () => {
      clearInterval(id)
      document.removeEventListener('visibilitychange', tick)
    }
  }, [fn, ms])
}
