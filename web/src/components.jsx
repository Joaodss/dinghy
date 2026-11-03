const STATUS_LABELS = {
  running: 'Running',
  partial: 'Partial',
  stopped: 'Stopped',
  error: 'Error',
}

export function StackBadge({ status }) {
  return <span class={`badge ${status}`}>{STATUS_LABELS[status] || status}</span>
}

/** Container state plus health, e.g. `running · healthy`. */
export function StateBadge({ c }) {
  let tone = 'stopped'
  if (c.state === 'running') tone = 'running'
  if (c.state === 'restarting' || c.health === 'unhealthy' || (c.state === 'exited' && c.exit_code)) tone = 'error'
  else if (c.health === 'starting' || c.state === 'paused') tone = 'partial'
  const exit = c.state === 'exited' && c.exit_code != null ? ` (${c.exit_code})` : ''
  return (
    <span class={`badge ${tone}`}>
      {c.state}
      {exit}
      {c.health ? ` · ${c.health}` : ''}
    </span>
  )
}

export function ErrorNote({ error }) {
  return error ? <p class="error">{error.message || String(error)}</p> : null
}
