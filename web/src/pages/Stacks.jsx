import { useCallback, useEffect, useState } from 'preact/hooks'
import { api } from '../api.js'
import { ErrorNote, StackBadge, StateBadge } from '../components.jsx'
import { formatBytes, useVisibleInterval } from '../util.js'

export function Stacks() {
  const [data, setData] = useState(null)
  const [error, setError] = useState(null)

  const load = useCallback(async () => {
    try {
      setData(await api('/api/stacks'))
      setError(null)
    } catch (err) {
      setError(err)
    }
  }, [])

  useEffect(() => {
    load()
  }, [load])
  useVisibleInterval(load, 15000)

  if (!data) return error ? <ErrorNote error={error} /> : <p class="muted">Loading…</p>

  const total = data.stacks.reduce((sum, s) => sum + (s.memory_bytes || 0), 0)
  return (
    <>
      <div class="page-head">
        <h1>Stacks</h1>
        <span class="muted">
          {data.stacks.length} stacks · {formatBytes(total)} in use
        </span>
        <span class="spacer" />
        <button onClick={load}>Refresh</button>
      </div>
      <ErrorNote error={error} />
      {data.stacks.length === 0 ? (
        <p class="empty">No stacks found. Each stack is a folder with a compose.yaml.</p>
      ) : (
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Stack</th>
                <th>Status</th>
                <th class="num">Containers</th>
                <th class="num">Memory</th>
              </tr>
            </thead>
            <tbody>
              {data.stacks.map((s) => (
                <tr key={s.name}>
                  <td>
                    <a href={`#/stacks/${s.name}`} class="strong">
                      {s.name}
                    </a>
                  </td>
                  <td>
                    <StackBadge status={s.status} />
                  </td>
                  <td class="num">
                    {s.running}/{s.total}
                  </td>
                  <td class="num">{formatBytes(s.memory_bytes)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {data.orphans.length > 0 && (
        <>
          <h2>Containers without a stack</h2>
          <div class="table-wrap">
            <table>
              <thead>
                <tr>
                  <th>Container</th>
                  <th>Image</th>
                  <th>State</th>
                  <th class="num">Memory</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {data.orphans.map((c) => (
                  <tr key={c.id}>
                    <td class="strong">
                      {c.name}
                      {c.project && <span class="muted small"> ({c.project})</span>}
                    </td>
                    <td class="mono small">{c.image}</td>
                    <td>
                      <StateBadge c={c} />
                    </td>
                    <td class="num">{formatBytes(c.memory_bytes)}</td>
                    <td class="num">
                      <a href={`#/logs/${c.id}?name=${encodeURIComponent(c.name)}`}>Logs</a>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}
    </>
  )
}
