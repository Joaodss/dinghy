import { render } from 'preact'
import { useEffect, useState } from 'preact/hooks'
import { api, setCsrf, setUnauthorizedHandler } from './api.js'
import { useRoute } from './util.js'
import { Stacks } from './pages/Stacks.jsx'
import { Stack } from './pages/Stack.jsx'
import { Logs } from './pages/Logs.jsx'
import { Files } from './pages/Files.jsx'
import './style.css'

function Login({ onLogin }) {
  const [password, setPassword] = useState('')
  const [error, setError] = useState(null)
  const [busy, setBusy] = useState(false)

  async function submit(e) {
    e.preventDefault()
    setBusy(true)
    setError(null)
    try {
      const res = await api('/api/login', { method: 'POST', body: { password } })
      setCsrf(res.csrf)
      onLogin()
    } catch (err) {
      setError(err.message)
      setBusy(false)
    }
  }

  return (
    <main class="login">
      <form onSubmit={submit} class="card">
        <h1 class="brand">dinghy</h1>
        <label>
          Password
          <input type="password" autoFocus autocomplete="current-password" value={password} onInput={(e) => setPassword(e.target.value)} />
        </label>
        {error && <p class="error">{error}</p>}
        <button class="primary" disabled={busy || !password}>
          {busy ? 'Signing in…' : 'Sign in'}
        </button>
      </form>
    </main>
  )
}

function Page({ route }) {
  const parts = route.path.split('/').filter(Boolean).map(decodeURIComponent)
  switch (parts[0]) {
    case undefined:
      return <Stacks />
    case 'stacks':
      return <Stack name={parts[1]} />
    case 'logs':
      return <Logs id={parts[1]} name={route.query.get('name')} stack={route.query.get('stack')} />
    case 'files':
      return <Files root={parts[1]} path={parts.slice(2).join('/')} />
    default:
      return <p class="empty">Page not found.</p>
  }
}

function App() {
  const route = useRoute()
  const [session, setSession] = useState(null)
  const [error, setError] = useState(null)

  async function load() {
    try {
      const s = await api('/api/session')
      setCsrf(s.csrf)
      setSession(s)
    } catch (err) {
      setError(err.status === 403 ? 'Cloudflare Access did not let this request through.' : err.message)
    }
  }

  useEffect(() => {
    setUnauthorizedHandler(() => setSession((s) => ({ ...s, authenticated: false })))
    load()
  }, [])

  async function logout() {
    await api('/api/logout', { method: 'POST' }).catch(() => {})
    setCsrf(null)
    setSession((s) => ({ ...s, authenticated: false }))
  }

  if (error) return <p class="error page-error">{error}</p>
  if (!session) return null
  if (!session.authenticated) return <Login onLogin={load} />

  const section = route.path.startsWith('/files') ? 'files' : 'stacks'
  return (
    <>
      <header class="topbar">
        <a href="#/" class="brand">
          dinghy
        </a>
        <nav>
          <a href="#/" class={section === 'stacks' ? 'active' : ''}>
            Stacks
          </a>
          <a href="#/files" class={section === 'files' ? 'active' : ''}>
            Files
          </a>
        </nav>
        <span class="spacer" />
        {session.email && <span class="muted small">{session.email}</span>}
        <button class="ghost" onClick={logout}>
          Sign out
        </button>
      </header>
      <main class="content">
        <Page route={route} />
      </main>
    </>
  )
}

render(<App />, document.getElementById('app'))
