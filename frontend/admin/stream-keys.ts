import { apiFetch } from './auth.js';
import { closeModal, confirmModal, openModal } from '../shared/components.js';
import { esc, toast } from '../shared/utils.js';
import type { StreamKey } from './types.js';

const INGEST_HOST = location.hostname;

interface SrtConfig {
  ingestPassphrase: string | null;
  playbackPassphrase: string | null;
  pbkeylen: number;
}

// Response of GET /api/stream-keys/:id/srt-playback (gh #226) — the same shape
// /api/watch/:slug hands Farbplay, minted by the same helper.
interface SrtPlayback {
  host: string;
  port: number;
  streamid: string;
  ttlSeconds: number;
}

const maskToken = (tok: string): string => '••••••••' + tok.slice(-4);

let keys: StreamKey[] = [];
let srtConfig: SrtConfig | null = null;
let onChange: () => void = () => {};

export function getStreamKeys(): StreamKey[] {
  return keys;
}

export function setOnChange(fn: () => void): void {
  onChange = fn;
}

export async function loadKeys(): Promise<void> {
  const [res, srtRes] = await Promise.all([
    apiFetch('/api/stream-keys'),
    apiFetch('/api/stream-keys/srt-config'),
  ]);
  if (!res) return;
  keys = await res.json();
  // srtConfig supplies the passphrases appended to the SRT URLs below; the
  // encryption toggle itself lives in the Settings tab (gh #208).
  srtConfig = srtRes && srtRes.ok ? await srtRes.json().catch(() => null) : null;
  renderKeys();
}

function renderKeys(): void {
  const container = document.getElementById('keys-list');
  if (!container) return;
  if (!keys.length) {
    container.innerHTML = '<div class="empty">No stream keys yet.</div>';
    return;
  }

  const proto = location.protocol === 'https:' ? 'https' : 'http';
  const wsproto = location.protocol === 'https:' ? 'wss' : 'ws';

  // Build every URL from a token so the masked variant (issue #204) reuses the
  // exact same shape — only the embedded key differs.
  const urlsFor = (tok: string): Record<string, string> => ({
    key: tok,
    srt: `srt://${INGEST_HOST}:9999?streamid=default/live/${tok}`,
    rtmp: `rtmp://${INGEST_HOST}:1935/live`,
    whip: `${proto}://${INGEST_HOST}/live/${tok}?direction=whip`,
    webrtc: `${wsproto}://${INGEST_HOST}/live/${tok}`,
    llhls: `${proto}://${INGEST_HOST}/live/${tok}/llhls.m3u8`,
  });

  container.innerHTML = keys
    .map((k) => {
      const full = urlsFor(k.key_token);
      const masked = urlsFor(maskToken(k.key_token));

      // SRT ingest encryption (opt-in): when a passphrase is set, the raw srt://
      // ingest URL (9999) must carry it. The passphrase is masked like the key —
      // revealed and copied in full via data-full. Playback (9998) gets its
      // passphrase in generateSrtPlayback, since that URL is minted on demand.
      const pbk = srtConfig?.pbkeylen ?? 16;
      const ingestPass = srtConfig?.ingestPassphrase || '';
      if (ingestPass) {
        full.srt += `&passphrase=${ingestPass}&pbkeylen=${pbk}`;
        masked.srt += `&passphrase=${maskToken(ingestPass)}&pbkeylen=${pbk}`;
      }

      // Rendered masked; the copy handler and Reveal toggle read data-full.
      const row = (label: string, field: string): string => `
        <div class="url-row">
          <span class="url-label">${esc(label)}</span>
          <input readonly class="url-input" data-full="${esc(full[field])}" data-masked="${esc(masked[field])}" style="font-family:monospace;font-size:11px" value="${esc(masked[field])}">
        </div>`;

      // SRT playback carries an expiring SignedPolicy token that only the backend
      // can sign (gh #226), so this row starts empty and is filled per click.
      const srtPlaybackRow = `
        <div class="url-row">
          <span class="url-label">SRT</span>
          <input readonly class="url-input" style="font-family:monospace;font-size:11px" placeholder="Signed URL — expires 5 min after generating" value="">
          <button class="btn btn-sm" data-action="gen-srt-play" data-id="${esc(k.id)}">Generate</button>
        </div>`;

      const blocked = !!k.blocked;
      return `
      <div class="key-card" data-revealed="false">
        <div class="key-card-header">
          <div class="key-card-name">${esc(k.name)}</div>
          ${blocked ? '<span class="badge badge-ended">Blocked</span>' : ''}
          ${k.room_names ? `<div class="key-card-rooms">Used in: ${esc(k.room_names)}</div>` : ''}
          ${blocked ? `<button class="btn btn-sm btn-primary" data-action="unblock-key" data-id="${esc(k.id)}" title="Re-allow ingest for this key after a stream kick">Allow ingest</button>` : ''}
          <button class="btn btn-sm" data-action="reveal-key">Reveal</button>
          <button class="btn btn-sm btn-danger" data-action="delete-key" data-id="${esc(k.id)}">Delete</button>
        </div>
        <div class="key-card-body">
          ${row('Stream Key', 'key')}
          <details class="url-group" open>
            <summary>Streaming URLs (ingest)</summary>
            ${row('SRT', 'srt')}
            ${row('RTMP', 'rtmp')}
            ${row('WHIP', 'whip')}
          </details>
          <details class="url-group">
            <summary>Playback URLs</summary>
            ${row('WebRTC', 'webrtc')}
            ${row('LLHLS', 'llhls')}
            ${srtPlaybackRow}
          </details>
        </div>
      </div>`;
    })
    .join('');
}

function openKeyModal(): void {
  (document.getElementById('key-name') as HTMLInputElement).value = '';
  openModal('key-modal');
}

async function saveKey(): Promise<void> {
  const name = (document.getElementById('key-name') as HTMLInputElement).value.trim();
  if (!name) {
    toast('Name required');
    return;
  }
  const res = await apiFetch('/api/stream-keys', {
    method: 'POST',
    body: JSON.stringify({ name }),
  });
  if (!res || !res.ok) {
    toast('Failed to create key');
    return;
  }
  closeModal('key-modal');
  toast('Stream key created');
  onChange();
}

async function unblockKey(id: string): Promise<void> {
  const res = await apiFetch(`/api/stream-keys/${id}/unblock`, { method: 'POST' });
  if (res && res.ok) {
    toast('Ingest re-enabled');
    onChange();
  } else {
    toast('Failed to re-enable ingest');
  }
}

// Mint a signed SRT playback URL for one key and drop it into its row (gh #226).
// OME's SignedPolicy rejects an unsigned streamid on the SRT publisher, and only
// the backend holds OME_SIGNED_POLICY_SECRET — so this can't be rendered with the
// card like the other URLs. Both variants are stashed on the input so Reveal and
// click-to-copy behave exactly as they do for every other row.
async function generateSrtPlayback(btn: HTMLElement): Promise<void> {
  const id = btn.getAttribute('data-id') || '';
  const input = btn.closest('.url-row')?.querySelector<HTMLInputElement>('input.url-input');
  if (!input) return;

  const res = await apiFetch(`/api/stream-keys/${id}/srt-playback`);
  if (!res || !res.ok) {
    toast('Failed to generate playback URL');
    return;
  }
  const play = (await res.json()) as SrtPlayback;

  // The streamid carries its own ?policy=&signature= query, so it has to be
  // percent-encoded inside the outer srt:// URL — left raw, a player reads them
  // as sibling SRT socket options and the handshake fails.
  let full = `srt://${play.host || INGEST_HOST}:${play.port}?streamid=${encodeURIComponent(play.streamid)}`;
  const tok = keys.find((k) => k.id === id)?.key_token;
  let masked = tok ? full.replaceAll(tok, maskToken(tok)) : full;

  const playbackPass = srtConfig?.playbackPassphrase || '';
  if (playbackPass) {
    const pbk = srtConfig?.pbkeylen ?? 16;
    full += `&passphrase=${playbackPass}&pbkeylen=${pbk}`;
    masked += `&passphrase=${maskToken(playbackPass)}&pbkeylen=${pbk}`;
  }

  input.dataset.full = full;
  input.dataset.masked = masked;
  const revealed = (btn.closest('.key-card') as HTMLElement | null)?.dataset.revealed === 'true';
  input.value = revealed ? full : masked;
  btn.textContent = 'Regenerate';
}

async function deleteKey(id: string): Promise<void> {
  if (
    !(await confirmModal({
      title: 'Delete Stream Key',
      message: 'Any encoder using this key will stop being able to ingest.',
      confirmLabel: 'Delete',
      danger: true,
    }))
  )
    return;
  const res = await apiFetch(`/api/stream-keys/${id}`, { method: 'DELETE' });
  if (res && res.ok) {
    toast('Key deleted');
    onChange();
  } else {
    toast('Delete failed');
  }
}

export function initStreamKeys(): void {
  document.getElementById('new-key-btn')?.addEventListener('click', openKeyModal);
  document
    .getElementById('key-modal-close')
    ?.addEventListener('click', () => closeModal('key-modal'));
  document
    .getElementById('key-modal-cancel')
    ?.addEventListener('click', () => closeModal('key-modal'));
  document.getElementById('key-modal-save')?.addEventListener('click', saveKey);
}

export function handleKeyAction(action: string, target: HTMLElement): void {
  if (action === 'delete-key') {
    const id = target.getAttribute('data-id') || '';
    void deleteKey(id);
    return;
  }
  if (action === 'unblock-key') {
    const id = target.getAttribute('data-id') || '';
    void unblockKey(id);
    return;
  }
  if (action === 'gen-srt-play') {
    void generateSrtPlayback(target);
    return;
  }
  if (action === 'reveal-key') {
    // Toggle every masked field in this card (issue #204). Copy still yields
    // the full value regardless — the copy handler prefers data-full.
    const card = target.closest('.key-card') as HTMLElement | null;
    if (!card) return;
    const reveal = card.dataset.revealed !== 'true';
    card.dataset.revealed = String(reveal);
    card.querySelectorAll<HTMLInputElement>('input.url-input').forEach((inp) => {
      inp.value = (reveal ? inp.dataset.full : inp.dataset.masked) || '';
    });
    target.textContent = reveal ? 'Hide' : 'Reveal';
  }
}
