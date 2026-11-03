import { useCallback, useEffect, useRef, useState } from 'preact/hooks'
import { api, q } from '../api.js'
import { ErrorNote } from '../components.jsx'
import { go } from '../util.js'

const parentOf = (path) => (path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : '')
const join = (dir, name) => (dir ? `${dir}/${name}` : name)

/** CodeMirror, mounted on demand. Recreated whenever `file.version` changes. */
function CodeEditor({ file, readOnly, onChange, onSave }) {
  const host = useRef(null)
  const saveRef = useRef(onSave)
  saveRef.current = onSave

  useEffect(() => {
    let editor
    let cancelled = false
    import('../editor.js').then(({ createEditor }) => {
      if (cancelled) return
      editor = createEditor(host.current, {
        doc: file.content,
        filename: file.path,
        readOnly,
        onChange,
        onSave: () => saveRef.current(),
      })
    })
    return () => {
      cancelled = true
      editor?.destroy()
    }
  }, [file.version, readOnly])

  return <div class="editor" ref={host} />
}

function TreeNode({ dir, entries, expanded, selected, onToggle, onSelect, depth }) {
  return (
    <ul class="tree">
      {(entries[dir] || []).map((e) => {
        const path = join(dir, e.name)
        const isDir = e.kind === 'dir'
        const open = expanded.has(path)
        return (
          <li key={path}>
            <button
              class={`tree-item ${selected === path ? 'selected' : ''} ${e.kind}`}
              style={{ paddingLeft: `${0.5 + depth * 0.9}rem` }}
              title={e.kind === 'symlink' ? 'Symlink that leaves this root or is broken' : path}
              onClick={() => (isDir ? onToggle(path) : onSelect(path, e.kind))}
            >
              <span class="icon">{isDir ? (open ? '▾' : '▸') : e.kind === 'symlink' ? '↗' : '·'}</span>
              {e.name}
            </button>
            {isDir && open && (
              <TreeNode dir={path} entries={entries} expanded={expanded} selected={selected} onToggle={onToggle} onSelect={onSelect} depth={depth + 1} />
            )}
          </li>
        )
      })}
    </ul>
  )
}

export function Files({ root, path }) {
  const [roots, setRoots] = useState(null)
  const [entries, setEntries] = useState({})
  const [expanded, setExpanded] = useState(new Set())
  const [selected, setSelected] = useState(null)
  const [selectedKind, setSelectedKind] = useState(null)
  const [file, setFile] = useState(null)
  const [fileError, setFileError] = useState(null)
  const [error, setError] = useState(null)
  const [dirty, setDirty] = useState(false)
  const [saving, setSaving] = useState(false)
  const [conflict, setConflict] = useState(false)
  const draft = useRef('')
  const saved = useRef('')
  const dirtyRef = useRef(false)
  dirtyRef.current = dirty

  useEffect(() => {
    api('/api/roots').then((r) => setRoots(r.roots), setError)
  }, [])

  useEffect(() => {
    if (roots && !root && roots.length) go(`/files/${roots[0]}`)
  }, [roots, root])

  useEffect(() => {
    const warn = (e) => {
      if (dirtyRef.current) e.preventDefault()
    }
    addEventListener('beforeunload', warn)
    return () => removeEventListener('beforeunload', warn)
  }, [])

  const loadDir = useCallback(
    async (dir) => {
      const list = await api(`/api/files?${q({ root, path: dir })}`)
      setEntries((prev) => ({ ...prev, [dir]: list }))
      return list
    },
    [root],
  )

  // A new root starts from a fresh tree.
  useEffect(() => {
    if (!root) return
    setEntries({})
    setExpanded(new Set())
    setFile(null)
    loadDir('').catch(setError)
  }, [root, loadDir])

  async function openFile(target, reveal = false) {
    setFileError(null)
    setConflict(false)
    try {
      const res = await api(`/api/files/content?${q({ root, path: target, reveal })}`)
      draft.current = res.content
      saved.current = res.content
      setDirty(false)
      setFile({ path: target, ...res, version: Date.now() })
    } catch (err) {
      setFile(null)
      setFileError(err)
    }
  }

  // Follow the URL: expand the path's folders, then open it if it is a file.
  useEffect(() => {
    if (!root || !path) return
    let cancelled = false
    ;(async () => {
      const parts = path.split('/')
      const dirs = parts.slice(0, -1).map((_, i) => parts.slice(0, i + 1).join('/'))
      try {
        for (const d of dirs) await loadDir(d)
        const list = await loadDir(parentOf(path))
        if (cancelled) return
        const entry = list.find((e) => e.name === parts[parts.length - 1])
        setExpanded((prev) => new Set([...prev, ...dirs, ...(entry?.kind === 'dir' ? [path] : [])]))
        setSelected(path)
        setSelectedKind(entry?.kind || null)
        if (entry?.kind === 'dir') await loadDir(path)
        else if (entry) await openFile(path)
        else setFileError(new Error(`${path} does not exist`))
      } catch (err) {
        if (!cancelled) setFileError(err)
      }
    })()
    return () => {
      cancelled = true
    }
  }, [root, path])

  function confirmDiscard() {
    return !dirtyRef.current || confirm('Discard unsaved changes?')
  }

  function toggle(dir) {
    setSelected(dir)
    setSelectedKind('dir')
    setExpanded((prev) => {
      const next = new Set(prev)
      if (next.has(dir)) next.delete(dir)
      else {
        next.add(dir)
        loadDir(dir).catch(setError)
      }
      return next
    })
  }

  function select(target, kind) {
    if (target === file?.path || !confirmDiscard()) return
    setDirty(false)
    setSelectedKind(kind)
    go(`/files/${root}/${target}`)
  }

  async function save() {
    if (!file || file.masked || saving || !dirtyRef.current) return
    setSaving(true)
    setFileError(null)
    try {
      const content = draft.current
      const { data } = await api(`/api/files/content?${q({ root, path: file.path })}`, {
        method: 'PUT',
        body: content,
        headers: { 'If-Match': file.etag, 'Content-Type': 'text/plain; charset=utf-8' },
        withHeaders: true,
      })
      setFile((f) => ({ ...f, etag: data.etag }))
      saved.current = content
      setDirty(draft.current !== content)
    } catch (err) {
      if (err.status === 412) setConflict(true)
      else setFileError(err)
    } finally {
      setSaving(false)
    }
  }

  function targetDir() {
    if (!selected) return ''
    return selectedKind === 'dir' ? selected : parentOf(selected)
  }

  async function create(dir) {
    const base = targetDir()
    const name = prompt(`New ${dir ? 'folder' : 'file'} in /${base}`)
    if (!name) return
    const target = join(base, name.replace(/^\/+/, ''))
    try {
      await api('/api/files/create', { method: 'POST', body: { root, path: target, dir } })
      await loadDir(parentOf(target))
      if (base) setExpanded((prev) => new Set([...prev, base]))
      if (!dir && confirmDiscard()) go(`/files/${root}/${target}`)
    } catch (err) {
      setError(err)
    }
  }

  async function rename() {
    if (!selected) return
    const current = selected.split('/').pop()
    const name = prompt(`Rename ${selected} to`, current)
    if (!name || name === current) return
    const to = join(parentOf(selected), name)
    try {
      await api('/api/files/rename', { method: 'POST', body: { root, from: selected, to } })
      await loadDir(parentOf(selected))
      if (file?.path === selected) {
        setDirty(false)
        go(`/files/${root}/${to}`)
      } else setSelected(to)
    } catch (err) {
      setError(err)
    }
  }

  if (!roots) return error ? <ErrorNote error={error} /> : <p class="muted">Loading…</p>

  return (
    <div class="files">
      <aside class="file-tree">
        <div class="tree-head">
          <select value={root} onChange={(e) => confirmDiscard() && go(`/files/${e.target.value}`)}>
            {roots.map((r) => (
              <option key={r} value={r}>
                {r}/
              </option>
            ))}
          </select>
          <button class="ghost small" title="New file" onClick={() => create(false)}>
            + File
          </button>
          <button class="ghost small" title="New folder" onClick={() => create(true)}>
            + Folder
          </button>
          <button class="ghost small" title="Rename selected" disabled={!selected} onClick={rename}>
            Rename
          </button>
        </div>
        <ErrorNote error={error} />
        <TreeNode dir="" entries={entries} expanded={expanded} selected={selected} onToggle={toggle} onSelect={select} depth={0} />
      </aside>

      <section class="file-pane">
        {file ? (
          <>
            <div class="file-head">
              <span class="mono strong">
                {root}/{file.path}
              </span>
              {dirty && <span class="badge partial">unsaved</span>}
              <span class="spacer" />
              {file.masked && (
                <>
                  <span class="muted small">Values hidden</span>
                  <button onClick={() => openFile(file.path, true)}>Show values</button>
                </>
              )}
              <button class="primary" disabled={!dirty || saving || file.masked} onClick={save}>
                {saving ? 'Saving…' : 'Save'}
              </button>
            </div>
            {conflict && (
              <p class="error">
                The file changed on disk since you opened it, so it was not saved.{' '}
                <button onClick={() => openFile(file.path)}>Reload from disk</button>
              </p>
            )}
            <ErrorNote error={fileError} />
            <CodeEditor
              file={file}
              readOnly={file.masked}
              onChange={(text) => {
                draft.current = text
                setDirty(text !== saved.current)
              }}
              onSave={save}
            />
          </>
        ) : fileError ? (
          <ErrorNote error={fileError} />
        ) : (
          <p class="empty">Pick a file to edit.</p>
        )}
      </section>
    </div>
  )
}
