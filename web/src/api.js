let csrf = null
let onUnauthorized = () => {}

export function setCsrf(token) {
  csrf = token
}

export function setUnauthorizedHandler(fn) {
  onUnauthorized = fn
}

export class ApiError extends Error {
  constructor(status, message) {
    super(message)
    this.status = status
  }
}

/**
 * JSON fetch against the dinghy API. Unsafe methods carry the CSRF header.
 * Returns `{ data, headers }` when `withHeaders` is set.
 */
export async function api(path, { method = 'GET', body, headers = {}, withHeaders = false } = {}) {
  const h = { ...headers }
  if (method !== 'GET' && csrf) h['X-Dinghy-CSRF'] = csrf
  let payload = body
  if (body !== undefined && typeof body !== 'string') {
    h['Content-Type'] = 'application/json'
    payload = JSON.stringify(body)
  }
  const res = await fetch(path, { method, headers: h, body: payload, credentials: 'same-origin' })
  if (res.status === 401 && path !== '/api/login') onUnauthorized()
  if (!res.ok) {
    let message = res.statusText
    try {
      message = (await res.json()).error || message
    } catch {
      // Not JSON (e.g. a proxy error page).
    }
    throw new ApiError(res.status, message)
  }
  const data = res.status === 204 ? null : await res.json()
  return withHeaders ? { data, headers: res.headers } : data
}

/**
 * Opens an SSE stream. `handlers` maps event names to callbacks receiving the
 * raw `data` string. Returns a function that closes the stream.
 */
export function stream(path, handlers) {
  const source = new EventSource(path)
  for (const [event, fn] of Object.entries(handlers)) {
    if (event === 'onerror' || event === 'onopen') continue
    source.addEventListener(event, (e) => fn(e.data))
  }
  source.onopen = () => handlers.onopen?.()
  source.onerror = () => {
    // EventSource reconnects on its own; callers decide whether to let it.
    handlers.onerror?.(source)
  }
  return () => source.close()
}

export const q = (params) => new URLSearchParams(params).toString()
