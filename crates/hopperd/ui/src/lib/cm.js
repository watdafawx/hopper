// CodeMirror, loaded only when a file is opened so the panel itself stays
// small.

import { basicSetup } from 'codemirror';
import { EditorView, keymap, Decoration } from '@codemirror/view';
import { EditorState, StateField, StateEffect } from '@codemirror/state';
import { StreamLanguage } from '@codemirror/language';
import { json } from '@codemirror/lang-json';
import { javascript } from '@codemirror/lang-javascript';
import { yaml } from '@codemirror/lang-yaml';
import { toml } from '@codemirror/legacy-modes/mode/toml';
import { properties } from '@codemirror/legacy-modes/mode/properties';
import { oneDark } from '@codemirror/theme-one-dark';

function language(name) {
  const ext = name.split('.').pop().toLowerCase();
  switch (ext) {
    case 'json': case 'json5': case 'mcmeta': return json();
    case 'js': case 'ts': case 'zs': return javascript();
    case 'yml': case 'yaml': return yaml();
    case 'toml': return StreamLanguage.define(toml);
    case 'properties': case 'cfg': case 'conf': case 'ini': case 'lang': return StreamLanguage.define(properties);
    default: return [];
  }
}

const look = EditorView.theme({
  '&': { height: '100%', fontSize: '12.5px', backgroundColor: 'var(--term)' },
  '.cm-scroller': { fontFamily: 'var(--mono)', lineHeight: '1.6' },
  '.cm-gutters': { backgroundColor: 'var(--term)', borderRight: '1px solid var(--line)' },
  '&.cm-focused': { outline: 'none' },
  '.cm-hopper-mark': { backgroundColor: '#f973162e', boxShadow: 'inset 3px 0 0 var(--acc)' },
}, { dark: true });

// The line a search hit or a setting opened at, tinted like desktop's
// config editor marks it (until the text is edited).
const markLine = StateEffect.define();
const lineMark = Decoration.line({ class: 'cm-hopper-mark' });
const marked = StateField.define({
  create: () => Decoration.none,
  update(deco, tr) {
    for (const e of tr.effects) if (e.is(markLine)) return e.value == null ? Decoration.none : Decoration.set([lineMark.range(e.value)]);
    return tr.docChanged ? Decoration.none : deco;
  },
  provide: (f) => EditorView.decorations.from(f),
});

/** An editor in `parent`. Ctrl+S calls onSave; edits call onChange(text). */
export function createEditor(parent, doc, name, { onSave, onChange }) {
  const view = new EditorView({
    parent,
    state: EditorState.create({
      doc,
      extensions: [
        keymap.of([{ key: 'Mod-s', preventDefault: true, run: () => (onSave(), true) }]),
        basicSetup,
        language(name),
        oneDark,
        look,
        marked,
        EditorView.lineWrapping,
        EditorView.updateListener.of((u) => u.docChanged && onChange(u.state.doc.toString())),
      ],
    }),
  });
  return {
    get: () => view.state.doc.toString(),
    set: (text) => view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: text } }),
    /** Puts the cursor on 1-based `line`, in the middle of the view; `mark` tints the line. */
    goto: (line, mark = false) => {
      const l = view.state.doc.line(Math.max(1, Math.min(line, view.state.doc.lines)));
      view.dispatch({
        selection: { anchor: l.from },
        effects: [EditorView.scrollIntoView(l.from, { y: 'center' }), markLine.of(mark ? l.from : null)],
      });
      view.focus();
    },
    focus: () => view.focus(),
    destroy: () => view.destroy(),
  };
}
