import { useCallback, useEffect, useRef, useState } from 'preact/hooks'
import { api, stream } from '../api.js'
import { ErrorNote, StackBadge, StateBadge } from '../components.jsx'
import { formatBytes, formatUptime, useVisibleInterval } from '../util.js'

const ACTIONS = [
  { action: 'up', label: 'Up' },
  { action: 'restart', label: 'Restart' },
  { action: 'pull', label: 'Pull + Up' },
  { action: 'down', label: 'Down', confirm: 'Stop and remove the containers of this stack?' },
]

/** Live output of one compose action. */
function OpOutput({ op, onDone }) {
  const [lines, setLines] = useState([])
  const [exit, setExit] = useState(null)
  const pre = useRef(null)

  useEffect(() => {
    setLines([])
    setExit(null)
    const close = stream(`/api/ops/${op.id}/stream`, {
      lines: (data) => setLines((prev) => prev.concat(JSON.parse(data))),
      done: (code) => {
        setExit(Number(code))
        close()
        onDone()
      },
      // The op is gone (expired or server restarted): stop reconnecting.
      onerror: () => close(),
    })
    return close
  }, [op.id])

  useEffect(() => {
    if (pre.current) pre.current.scrollTop = pre.current.scrollHeight
  }, [lines])

  return (
    <section class="op">
      <div class="op-head">
        <strong>{op.label}</strong>
        {exit === null ? (
          <span class="badge partial">running…</span>
        ) : exit === 0 ? (
          <span class="badge running">done</span>
        ) : (
          <span class="badge error">failed ({exit})</span>
        )}
      </div>
      <pre ref={pre} class="console">
        {lines.join('\n')}
      </pre>
    </section>
  )
}

export function Stack({ name }) {
  const [data, setData] = useState(null)
  const [error, setError] = useState(null)
  const [op, setOp] = useState(null)
  const [running, setRunning] = useState(false)

  const load = useCallback(async () => {
    try {
      setData(await api(`/api/stacks/${encodeURIComponent(name)}`))
      setError(null)
    } catch (err) {
      setError(err)
    }
  }, [name])

  useEffect(() => {
    setData(null)
    setOp(null)
    load()
  }, [load])
  useVisibleInterval(load, 15000)

  async function run({ action, label, confirm }) {
    if (confirm && !window.confirm(confirm)) return
    try {
      setRunning(true)
      const res = await api(`/api/stacks/${encodeURIComponent(name)}/${action}`, { method: 'POST' })
      setOp({ id: res.op, label: `${label} ${name}` })
    } catch (err) {
      setRunning(false)
      setError(err)
    }
  }

  const onDone = useCallback(() => {
    setRunning(false)
    load()
  }, [load])

  if (!data) return error ? <ErrorNote error={error} /> : <p class="muted">Loading…</p>

  const editBase = `#/files/stacks/${name}`
  return (
    <>
      <div class="page-head">
        <a href="#/" class="muted">
          Stacks
        </a>
        <span class="muted">/</span>
        <h1>{data.name}</h1>
        <StackBadge status={data.status} />
        <span class="spacer" />
        <a class="button" href={`${editBase}/${data.compose_file}`}>
          Edit {data.compose_file}
        </a>
        {data.has_env && (
          <a class="button" href={`${editBase}/.env`}>
            Edit .env
          </a>
        )}
      </div>

      <div class="actions">
        {ACTIONS.map((a) => (
          <button key={a.action} class={a.action === 'down' ? 'danger' : ''} disabled={running} onClick={() => run(a)}>
            {a.label}
          </button>
        ))}
        <span class="spacer" />
        <button class="ghost" onClick={load}>
          Refresh
        </button>
      </div>

      <ErrorNote error={error} />

      {data.containers.length === 0 ? (
        <p class="empty">No containers. Press Up to start this stack.</p>
      ) : (
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Container</th>
                <th>Image</th>
                <th>State</th>
                <th class="num">Uptime</th>
                <th class="num">Memory</th>
                <th>Ports</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {data.containers.map((c) => (
                <tr key={c.id}>
                  <td>
                    <div class="strong">{c.service || c.name}</div>
                    {c.service && <div class="muted small mono">{c.name}</div>}
                  </td>
                  <td class="mono small">{c.image}</td>
                  <td>
                    <StateBadge c={c} />
                  </td>
                  <td class="num">{c.state === 'running' ? formatUptime(c.started_at) : '—'}</td>
                  <td class="num">
                    {formatBytes(c.memory_bytes)}
                    {c.memory_limit && <span class="muted"> / {formatBytes(c.memory_limit)}</span>}
                  </td>
                  <td class="mono small">
                    {c.ports.map((p) => (
                      <div key={p}>{p}</div>
                    ))}
                  </td>
                  <td class="num">
                    <a href={`#/logs/${c.id}?name=${encodeURIComponent(c.name)}&stack=${encodeURIComponent(name)}`}>Logs</a>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {op && <OpOutput op={op} onDone={onDone} />}
    </>
  )
}
