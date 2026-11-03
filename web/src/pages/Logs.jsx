import { Component } from 'preact'
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'preact/hooks'
import { stream } from '../api.js'

const MAX_LINES = 5000
// Lines live in fixed blocks. A full block never changes again, so it is never
// re-rendered, and the browser skips laying out blocks that are off screen.
const BLOCK = 250
const MAX_BLOCKS = MAX_LINES / BLOCK

const pad = (n) => (n < 10 ? '0' : '') + n

/**
 * Splits Docker's RFC 3339 prefix off a line and shows it as local HH:MM:SS.
 * Formatting by hand: `toLocaleTimeString` costs ~100 µs a call, which a busy
 * container turns into most of a second per second.
 */
function splitTimestamp(text) {
  const space = text.indexOf(' ')
  if (space > 19 && text[4] === '-' && text[10] === 'T') {
    const ms = Date.parse(text.slice(0, space))
    if (!isNaN(ms)) {
      const d = new Date(ms)
      return [`${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`, text.slice(space + 1)]
    }
  }
  return ['', text]
}

function toLine(raw) {
  const [time, text] = splitTimestamp(raw.t)
  return { err: raw.s === 'e', time, text, lower: text.toLowerCase() }
}

const matches = (line, needle) => !needle || line.lower.includes(needle)

let nextBlockId = 0

/** Appends lines to the block list, copying only the last block. */
function appendLines(blocks, lines) {
  const next = blocks.slice()
  let i = 0
  const last = next[next.length - 1]
  if (last && last.lines.length < BLOCK) {
    const take = lines.slice(0, BLOCK - last.lines.length)
    next[next.length - 1] = { id: last.id, lines: last.lines.concat(take) }
    i = take.length
  }
  for (; i < lines.length; i += BLOCK) next.push({ id: nextBlockId++, lines: lines.slice(i, i + BLOCK) })
  return next.length > MAX_BLOCKS + 1 ? next.slice(next.length - MAX_BLOCKS - 1) : next
}

class LogBlock extends Component {
  shouldComponentUpdate(next) {
    return next.lines !== this.props.lines || next.needle !== this.props.needle
  }

  render({ lines, needle }) {
    const shown = needle ? lines.filter((l) => matches(l, needle)) : lines
    if (!shown.length) return null
    // Off-screen blocks are sized from their line count until first drawn
    // (exact unless lines wrap), so the scrollbar stays honest.
    return (
      <div class="log-block" style={{ containIntrinsicSize: `auto ${shown.length}lh` }}>
        {shown.map((l, i) => (
          <div key={i} class={l.err ? 'line err' : 'line'}>
            {l.time && <span class="ts">{l.time}</span>}
            {l.text}
          </div>
        ))}
      </div>
    )
  }
}

export function Logs({ id, name, stack }) {
  const [blocks, setBlocks] = useState([])
  const [status, setStatus] = useState('connecting')
  const [filter, setFilter] = useState('')
  const [paused, setPaused] = useState(false)
  const [queued, setQueued] = useState(0)
  const [attempt, setAttempt] = useState(0)
  const pausedRef = useRef(false)
  const queue = useRef([])
  const frame = useRef(0)
  const box = useRef(null)
  const stick = useRef(true)

  pausedRef.current = paused

  // At most one render per animation frame, however fast lines arrive. Hidden
  // tabs get no frames, so nothing is rendered until the tab is visible again.
  function schedule() {
    if (frame.current) return
    frame.current = requestAnimationFrame(() => {
      frame.current = 0
      if (pausedRef.current) {
        setQueued(Math.min(queue.current.length, MAX_LINES))
        return
      }
      // Only the newest MAX_LINES could ever be shown, so only those are formatted.
      const lines = queue.current.slice(-MAX_LINES).map(toLine)
      queue.current = []
      setQueued(0)
      if (lines.length) setBlocks((prev) => appendLines(prev, lines))
    })
  }

  useEffect(() => {
    setBlocks([])
    queue.current = []
    setStatus('connecting')
    // The server stops reading Docker's log stream as soon as this closes.
    const close = stream(`/api/containers/${encodeURIComponent(id)}/logs?tail=200`, {
      onopen: () => setStatus('live'),
      lines: (data) => {
        const raw = JSON.parse(data)
        queue.current = queue.current.length ? queue.current.concat(raw) : raw
        if (queue.current.length > 2 * MAX_LINES) queue.current = queue.current.slice(-MAX_LINES)
        schedule()
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
    return () => {
      close()
      cancelAnimationFrame(frame.current)
      frame.current = 0
    }
  }, [id, attempt])

  useEffect(() => {
    if (!paused) schedule()
  }, [paused])

  const needle = filter.trim().toLowerCase()
  const anyVisible = useMemo(() => blocks.some((b) => b.lines.some((l) => matches(l, needle))), [blocks, needle])

  useLayoutEffect(() => {
    const el = box.current
    if (el && stick.current) el.scrollTop = el.scrollHeight
  }, [blocks, needle])

  function onScroll() {
    const el = box.current
    stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40
  }

  function clear() {
    queue.current = []
    setQueued(0)
    setBlocks([])
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
        <button onClick={() => setPaused((p) => !p)}>{paused ? `Resume (${queued})` : 'Pause'}</button>
        <button onClick={clear}>Clear</button>
        {(status === 'disconnected' || status === 'ended') && <button onClick={() => setAttempt((a) => a + 1)}>Reconnect</button>}
      </div>
      <div class="console logs" ref={box} onScroll={onScroll}>
        {blocks.map((b) => (
          <LogBlock key={b.id} lines={b.lines} needle={needle} />
        ))}
        {!anyVisible && <div class="muted">{needle ? 'No lines match the filter.' : 'No output yet.'}</div>}
      </div>
    </div>
  )
}
