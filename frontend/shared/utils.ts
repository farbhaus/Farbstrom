export function esc(str: unknown): string {
  return String(str)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

// Autolink URLs in user-typed text. This is an escaping problem before it is a
// formatting one: the caller is about to write the result into innerHTML, so
// every part of it — the surrounding text, the link body and the href — goes
// through esc(). The pattern matches only http/https/www, so no `javascript:`
// or `data:` URL can ever reach an href.
const LINK_RE = /(?:https?:\/\/|www\.)[^\s<>"'`]+/gi;
// After trimming, a match must still be a scheme (or www.) plus at least one
// more character to count as a link — "https://" and "www." alone are not.
const LINK_OK = /^(?:https?:\/\/|www\.)[^\s]/i;

// Trailing punctuation is nearly always the sentence's, not the URL's:
// "have a look at https://example.com." A closing bracket belongs to the link
// only when the link opened it — Wikipedia URLs really do end in ")".
const CLOSERS: Record<string, string> = { ')': '(', ']': '[', '}': '{' };

function trimUrlTail(url: string): string {
  let end = url.length;
  while (end > 0) {
    const ch = url[end - 1]!;
    // Includes the smart quotes and ellipsis macOS substitutes while typing.
    if ('.,;:!?"\'…‘’“”«»'.includes(ch)) {
      end--;
      continue;
    }
    const open = CLOSERS[ch];
    if (open) {
      const body = url.slice(0, end);
      const opened = body.split(open).length - 1;
      const closed = body.split(ch).length - 1;
      if (closed > opened) {
        end--;
        continue;
      }
    }
    break;
  }
  return url.slice(0, end);
}

export function linkify(text: unknown): string {
  const src = String(text);
  let out = '';
  let cursor = 0;
  LINK_RE.lastIndex = 0;
  for (let m = LINK_RE.exec(src); m; m = LINK_RE.exec(src)) {
    const raw = trimUrlTail(m[0]);
    // Resume scanning right after what we consumed, so punctuation we handed
    // back to the text can't be re-matched as the start of another link.
    LINK_RE.lastIndex = m.index + Math.max(raw.length, 1);
    if (!LINK_OK.test(raw)) continue;
    const href = raw.toLowerCase().startsWith('www.') ? 'https://' + raw : raw;
    out += esc(src.slice(cursor, m.index));
    out += `<a href="${esc(href)}" target="_blank" rel="noopener noreferrer">${esc(raw)}</a>`;
    cursor = m.index + raw.length;
  }
  return out + esc(src.slice(cursor));
}

let toastTimer: ReturnType<typeof setTimeout> | null = null;

export function toast(msg: string, dur = 2500): void {
  let el = document.getElementById('toast');
  if (!el) {
    el = document.createElement('div');
    el.id = 'toast';
    document.body.appendChild(el);
  }
  el.textContent = msg;
  el.classList.add('show');
  if (toastTimer) clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el!.classList.remove('show'), dur);
}

export function copyToClipboard(text: string): void {
  navigator.clipboard.writeText(text).then(() => toast('Copied'));
}

export function fmtDate(iso: string | null | undefined): string | null {
  if (!iso) return null;
  return new Date(iso).toLocaleDateString('en-GB', {
    day: 'numeric',
    month: 'short',
    year: 'numeric',
  });
}

// Parse a SQLite "YYYY-MM-DD HH:MM:SS" UTC string (no zone marker) into a Date.
// Real ISO strings (with T and Z) are passed through.
export function parseDbDate(s: string | null | undefined): Date | null {
  if (!s) return null;
  return /[TZ]/.test(s) ? new Date(s) : new Date(s.replace(' ', 'T') + 'Z');
}

export function fmtDateTime(s: string | null | undefined): string | null {
  const d = parseDbDate(s);
  if (!d) return null;
  return d.toLocaleString('en-GB', {
    day: 'numeric',
    month: 'short',
    year: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}

export function fmtBytes(bytes: number): string {
  if (bytes >= 1_073_741_824) return (bytes / 1_073_741_824).toFixed(1) + ' GB';
  if (bytes >= 1_048_576) return (bytes / 1_048_576).toFixed(1) + ' MB';
  if (bytes >= 1024) return (bytes / 1024).toFixed(1) + ' KB';
  return bytes + ' B';
}

export function fmtBitrate(bps: number | undefined | null): string {
  if (!bps) return '—';
  if (bps >= 1_000_000) return (bps / 1_000_000).toFixed(1) + ' Mbps';
  return Math.round(bps / 1000) + ' kbps';
}

// Bits-per-second from a bytes/sec input. Used for network display, since
// link capacity (e.g. 1 Gbps) is conventionally measured in bits.
export function fmtBitRate(bytesPerSec: number): string {
  const bps = (bytesPerSec || 0) * 8;
  if (bps >= 1e9) return (bps / 1e9).toFixed(2) + ' Gbps';
  if (bps >= 1e6) return (bps / 1e6).toFixed(1) + ' Mbps';
  if (bps >= 1e3) return (bps / 1e3).toFixed(0) + ' kbps';
  return bps + ' bps';
}

export function fmtDuration(secs: number): string {
  secs = Math.floor(secs || 0);
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (d > 0) return `up ${d}d ${h}h`;
  if (h > 0) return `up ${h}h ${m}m`;
  return `up ${m}m`;
}

export function fmtUptime(iso: string): string {
  const secs = Math.floor((Date.now() - new Date(iso).getTime()) / 1000);
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ${secs % 60}s`;
  return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
}

export function pctClass(p: number): 'ok' | 'warn' | 'crit' {
  if (p > 85) return 'crit';
  if (p > 60) return 'warn';
  return 'ok';
}

export function fmtSourceIp(url: string | null | undefined): string {
  if (!url) return '';
  const m = url.match(/\/\/([^:/]+)/);
  return m && m[1] ? m[1] : '';
}
