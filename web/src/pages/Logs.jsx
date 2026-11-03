import { useEffect, useMemo, useRef, useState } from 'preact/hooks'
import { stream } from '../api.js'

const MAX_LINES = 5000

function splitTimestamp(text) {
  const space = text.indexOf(' ')
  if (space > 0 && /^\d{4}-\d\d-\d\dT/.test(text)) {
    const date = new Date(text.slice(0, space))
    if (!isNaN(date)) return [date.toLocaleTimeString([], { hour12: false }), text.slice(space + 1)]
  }
  return ['', text]
}

let nextKey = 0

export function Logs({ id, name, stack }) {
  const [lines, setLines] = useState([])
  const [status, setStatus] = useState('connecting')
  const [filter, setFilter] = useState('')
  const [paused, setPaused] = useState(false)
  const [attempt, setAttempt] = useState(0)
  const pausedRef = useRef(false)
  const pending = useRef([])
  const box = useRef(null)
  const stick = useRef(true)

  pausedRef.current = paused

  useEffect(() => {
    setLines([])
    pending.current = []
    setStatus('connecting')
    // The server stops reading Docker's log stream as soon as this closes.
    const close = stream(`/api/containers/${encodeURIComponent(id)}/logs?tail=200`, {
      onopen: () => setStatus('live'),
      lines: (data) => {
        const incoming = JSON.parse(data).map((l) => {
          const [time, text] = splitTimestamp(l.t)
          return { key: nextKey++, err: l.s === 'e', time, text }
        })
        if (pausedRef.current) {
          pending.current = pending.current.concat(incoming).slice(-MAX_LINES)
        } else {
          setLines((prev) => prev.concat(incoming).slice(-MAX_LINES))
        }
      },
      end: () => {
        setStatus('ended')
        close()
      },
      onerror: () => {
        // Reconnecting would replay the tail, so stop and let the user retry.
        setStatus((s) => (s === 'ended' ? s : 'disconnected'))
        close()
      },
    })
    return close
  }, [id, attempt])

  useEffect(() => {
    if (!paused && pending.current.length) {
      const queued = pending.current
      pending.current = []
      setLines((prev) => prev.concat(queued).slice(-MAX_LINES))
    }
  }, [paused])

  const visible = useMemo(() => {
    const needle = filter.trim().toLowerCase()
    return needle ? lines.filter((l) => l.text.toLowerCase().includes(needle)) : lines
  }, [lines, filter])

  useEffect(() => {
    const el = box.current
    if (el && stick.current) el.scrollTop = el.scrollHeight
  }, [visible])

  function onScroll() {
    const el = box.current
    stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40
  }

  return (
    <div class="logs-page">
      <div class="page-head">
        {stack ? (
          <a href={`#/stacks/${stack}`} class="muted">
            {stack}
          </a>
        ) : (
          <a href="#/" class="muted">
            Stacks
          </a>
        )}
        <span class="muted">/</span>
        <h1>{name || id.slice(0, 12)}</h1>
        <span class={`badge ${status === 'live' ? 'running' : status === 'connecting' ? 'partial' : 'stopped'}`}>{status}</span>
        <span class="spacer" />
        <input class="filter" type="search" placeholder="Filter" value={filter} onInput={(e) => setFilter(e.target.value)} />
        <button onClick={() => setPaused((p) => !p)}>{paused ? `Resume (${pending.current.length})` : 'Pause'}</button>
        <button onClick={() => setLines([])}>Clear</button>
        {(status === 'disconnected' || status === 'ended') && <button onClick={() => setAttempt((a) => a + 1)}>Reconnect</button>}
      </div>
      <div class="console logs" ref={box} onScroll={onScroll}>
        {visible.map((l) => (
          <div key={l.key} class={l.err ? 'line err' : 'line'}>
            {l.time && <span class="ts">{l.time}</span>}
            {l.text}
          </div>
        ))}
        {visible.length === 0 && <div class="muted">{filter ? 'No lines match the filter.' : 'No output yet.'}</div>}
      </div>
    </div>
  )
}
