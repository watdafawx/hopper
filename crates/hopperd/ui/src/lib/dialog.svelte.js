// One small modal for questions: ask() for text, confirmIt() for yes/no.

export const dialog = $state({ open: false, title: '', body: '', input: null, ok: 'OK', danger: false });

let settle = null;

function show(opts) {
  if (settle) settle(null);
  Object.assign(dialog, { open: true, body: '', input: null, ok: 'OK', danger: false }, opts);
  return new Promise((resolve) => (settle = resolve));
}

/** Asks for text; resolves to the text, or null if cancelled. */
export function ask(title, value = '', { body = '', ok = 'OK', placeholder = '' } = {}) {
  return show({ title, body, ok, input: { value, placeholder } });
}

/** Asks yes or no; resolves to true or false. */
export async function confirmIt(title, { body = '', ok = 'OK', danger = false } = {}) {
  return (await show({ title, body, ok, danger })) === true;
}

export function answer(value) {
  dialog.open = false;
  const s = settle;
  settle = null;
  s?.(value);
}
