import { apiFetch, fetchPreviewToken } from './auth.js';
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
let lastRendered = '';
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
  // The admin reloads everything every 15 s. Re-rendering unchanged keys
  // collapsed an open section and blanked every playback URL the admin had
  // just generated, so only redraw when something actually changed.
  const snapshot = JSON.stringify([keys, srtConfig]);
  if (snapshot === lastRendered) return;
  lastRendered = snapshot;
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

  // Build every ingest URL from a token so the masked variant (issue #204)
  // reuses the exact same shape — only the embedded key differs. Playback URLs
  // are not built here: OME publishes under the key's playback id, never the
  // key, and every playback URL needs a credential minted on demand.
  const urlsFor = (tok: string): Record<string, string> => ({
    key: tok,
    srt: `srt://${INGEST_HOST}:9999?streamid=default/live/${tok}`,
    rtmp: `rtmp://${INGEST_HOST}:1935/live`,
    whip: `${proto}://${INGEST_HOST}/live/${tok}?direction=whip`,
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
          <input readonly class="url-input url-input-dense" data-full="${esc(full[field])}" data-masked="${esc(masked[field])}" value="${esc(masked[field])}">
        </div>`;

      // Every playback URL carries a credential only the backend can mint — a
      // preview token for WebRTC / LL-HLS (OME asks the backend before each
      // browser session), a SignedPolicy signature for SRT (gh #226) — so these
      // rows start empty and are filled per click.
      const playbackRow = (label: string, action: string, kind: string): string => `
        <div class="url-row">
          <span class="url-label">${label}</span>
          <input readonly class="url-input url-input-dense" placeholder="Signed URL — expires 5 min after generating" value="">
          <button class="btn btn-sm" data-action="${action}" data-id="${esc(k.id)}" data-kind="${kind}">Generate</button>
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
            ${playbackRow('WebRTC', 'gen-browser-play', 'webrtc')}
            ${playbackRow('LLHLS', 'gen-browser-play', 'llhls')}
            ${playbackRow('SRT', 'gen-srt-play', 'srt')}
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
  // as sibling SRT socket options and the handshake fails. The stream name in
  // it is the playback id, which is no secret; the signature is what's masked.
  const srtUrl = (streamid: string): string =>
    `srt://${play.host || INGEST_HOST}:${play.port}?streamid=${encodeURIComponent(streamid)}`;
  let full = srtUrl(play.streamid);
  let masked = srtUrl(
    play.streamid.replace(/signature=([^&]+)/, (_m, sig: string) => `signature=${maskToken(sig)}`),
  );

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

// Fill a WebRTC or LL-HLS playback row for one key. Browser playback is
// admission-gated, so the URL carries a preview token (`?apt=`) that OME checks
// with the backend when a session starts; the token is what Reveal hides.
async function generateBrowserPlayback(btn: HTMLElement): Promise<void> {
  const key = keys.find((k) => k.id === btn.getAttribute('data-id'));
  const input = btn.closest('.url-row')?.querySelector<HTMLInputElement>('input.url-input');
  if (!key || !input) return;

  const token = await fetchPreviewToken();
  if (!token) {
    toast('Failed to generate playback URL');
    return;
  }
  const base =
    btn.getAttribute('data-kind') === 'llhls'
      ? `${location.protocol}//${location.host}/live/${key.playback_id}/llhls.m3u8`
      : `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/live/${key.playback_id}`;
  const full = `${base}?apt=${encodeURIComponent(token)}`;
  input.dataset.full = full;
  input.dataset.masked = `${base}?apt=${maskToken(token)}`;
  const revealed = (btn.closest('.key-card') as HTMLElement | null)?.dataset.revealed === 'true';
  input.value = revealed ? full : input.dataset.masked;
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
  if (action === 'gen-browser-play') {
    void generateBrowserPlayback(target);
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
