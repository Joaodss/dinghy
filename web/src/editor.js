// Loaded only on the Files page, so CodeMirror stays out of the main bundle.
import { EditorView, basicSetup } from 'codemirror'
import { Compartment, EditorState, Prec } from '@codemirror/state'
import { keymap } from '@codemirror/view'
import { StreamLanguage } from '@codemirror/language'
import { yaml } from '@codemirror/lang-yaml'
import { javascript } from '@codemirror/lang-javascript'
import { json } from '@codemirror/lang-json'
import { shell } from '@codemirror/legacy-modes/mode/shell'
import { properties } from '@codemirror/legacy-modes/mode/properties'
import { oneDark } from '@codemirror/theme-one-dark'

function languageFor(filename) {
  const name = filename.toLowerCase().split('/').pop()
  const ext = name.includes('.') ? name.split('.').pop() : ''
  if (name === '.env' || name.startsWith('.env.') || ext === 'env' || ['properties', 'ini', 'conf', 'cfg'].includes(ext)) {
    return StreamLanguage.define(properties)
  }
  switch (ext) {
    case 'yaml':
    case 'yml':
      return yaml()
    case 'json':
      return json()
    case 'js':
    case 'mjs':
    case 'cjs':
      return javascript()
    case 'ts':
      return javascript({ typescript: true })
    case 'sh':
    case 'bash':
      return StreamLanguage.define(shell)
  }
  if (name === 'dockerfile' || name === 'caddyfile' || !ext) return StreamLanguage.define(shell)
  return []
}

export function createEditor(parent, { doc, filename, readOnly, onChange, onSave }) {
  const readOnlyConf = new Compartment()
  const dark = matchMedia('(prefers-color-scheme: dark)').matches
  const view = new EditorView({
    parent,
    state: EditorState.create({
      doc,
      extensions: [
        Prec.high(keymap.of([{ key: 'Mod-s', preventDefault: true, run: () => (onSave(), true) }])),
        basicSetup,
        languageFor(filename),
        dark ? oneDark : [],
        readOnlyConf.of([EditorState.readOnly.of(readOnly), EditorView.editable.of(!readOnly)]),
        EditorView.updateListener.of((u) => {
          if (u.docChanged) onChange(u.state.doc.toString())
        }),
      ],
    }),
  })
  return {
    focus: () => view.focus(),
    destroy: () => view.destroy(),
  }
}
