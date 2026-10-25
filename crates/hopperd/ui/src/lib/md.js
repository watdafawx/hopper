// Markdown to HTML for changelogs: headings, lists, bold, italic, code and
// links. Everything is escaped first, and links must be http(s), so text
// from mod pages can't inject markup.

const esc = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

function inline(s) {
  return s
    // Markdown escapes ("ja\_jp") show as the character.
    .replace(/\\([\\`*_{}[\]()#+\-.!|])/g, '$1')
    // Links written as [text](<url>).
    .replace(/\(&lt;(https?:\/\/[^\s&]+)&gt;\)/g, '($1)')
    .replace(/`([^`]+)`/g, '<code>$1</code>')
    .replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g, '<a href="$2" target="_blank" rel="noreferrer">$1</a>')
    // Bold and italic never cross a tag, so they can't break the link above.
    .replace(/\*\*([^*<>]+)\*\*/g, '<b>$1</b>')
    .replace(/(^|[\s(>])\*([^*\s<>][^*<>]*)\*/g, '$1<i>$2</i>');
}

export function md(text) {
  const out = [];
  let list = false;
  const close = () => {
    if (list) out.push('</ul>');
    list = false;
  };
  for (const raw of esc(text || '').split(/\r?\n/)) {
    const line = raw.trimEnd();
    const h = line.match(/^(#{1,6})\s+(.*)$/);
    const li = line.match(/^\s*[-*+]\s+(.*)$/) || line.match(/^\s*\d+[.)]\s+(.*)$/);
    if (h) {
      close();
      const n = Math.min(h[1].length + 2, 6);
      out.push(`<h${n}>${inline(h[2])}</h${n}>`);
    } else if (li) {
      if (!list) out.push('<ul>');
      list = true;
      out.push(`<li>${inline(li[1])}</li>`);
    } else if (!line.trim()) {
      close();
    } else {
      close();
      out.push(`<p>${inline(line)}</p>`);
    }
  }
  close();
  return out.join('');
}
