import { apiFetch } from './auth.js';
import { closeModal, confirmModal, openModal } from '../shared/components.js';
import {
  esc,
  fmtBitRate,
  fmtBitrate,
  fmtBytes,
  fmtDuration,
  fmtSourceIp,
  fmtUptime,
  pctClass,
  toast,
} from '../shared/utils.js';
import type { MetricsResponse, OmeData } from './types.js';

let metricsData: MetricsResponse | null = null;
let omeData: OmeData | null = null;
let dashTickerId: ReturnType<typeof setInterval> | null = null;
let previewPlayer: OvenPlayerInstance | null = null;

let getActiveTab: () => string = () => '';
let onStreamKicked: () => void = () => {};

export function configureDashboard(opts: {
  getActiveTab: () => string;
  onStreamKicked: () => void;
}): void {
  getActiveTab = opts.getActiveTab;
  onStreamKicked = opts.onStreamKicked;
}

export async function loadDashboard(): Promise<void> {
  const res = await apiFetch('/api/admin/metrics');
  if (!res || !res.ok) return;
  metricsData = await res.json();
  renderDashboard();
}

export async function loadOme(): Promise<void> {
  const res = await apiFetch('/api/ome/status');
  if (!res) return;
  omeData = await res.json();
  if (getActiveTab() === 'ome') renderOme();
}

export function startDashboardTicker(): void {
  if (dashTickerId) return;
  let tick = 0;
  dashTickerId = setInterval(() => {
    if (getActiveTab() !== 'ome') return;
    void loadDashboard();
    // Refresh OME stream list every 3rd tick (~4.5s) — heavier query.
    if (tick % 3 === 0) void loadOme();
    tick++;
  }, 1500);
}

export function stopDashboardTicker(): void {
  if (dashTickerId) {
    clearInterval(dashTickerId);
    dashTickerId = null;
  }
}

function setText(id: string, text: string): void {
  const el = document.getElementById(id);
  if (el) el.textContent = text;
}

function renderDashboard(): void {
  if (!metricsData) return;
  const { cpu, memory, network, loadavg, uptime_secs } = metricsData;

  // CPU
  const cpuPct = cpu.percent || 0;
  setText('stat-cpu-value', cpuPct.toFixed(1) + '%');
  const cpuBar = document.getElementById('stat-cpu-bar') as HTMLElement | null;
  if (cpuBar) {
    cpuBar.style.width = Math.min(100, cpuPct) + '%';
    cpuBar.className = 'stat-bar-fill ' + pctClass(cpuPct);
  }
  const coresEl = document.getElementById('stat-cpu-cores');
  if (coresEl) {
    const cores = cpu.cores || [];
    coresEl.innerHTML = cores
      .map(
        (p, i) => `
        <div class="cpu-core">
          <div class="cpu-core-label"><span>${i}</span><span>${p.toFixed(0)}%</span></div>
          <div class="stat-bar stat-bar-tight"><div class="stat-bar-fill ${pctClass(p)}" style="width:${Math.min(100, p)}%"></div></div>
        </div>`,
      )
      .join('');
  }

  // Memory
  const memPct = memory.percent || 0;
  setText('stat-mem-value', memPct.toFixed(1) + '%');
  setText(
    'stat-mem-sub',
    `${fmtBytes(memory.used_bytes)} used · ${fmtBytes(memory.cached_bytes)} cache · ${fmtBytes(memory.total_bytes)} total`,
  );
  const total = memory.total_bytes || 1;
  const setSeg = (id: string, bytes: number) => {
    const el = document.getElementById(id);
    if (el) (el as HTMLElement).style.width = ((bytes / total) * 100).toFixed(2) + '%';
  };
  setSeg('stat-mem-seg-used', memory.used_bytes);
  setSeg('stat-mem-seg-buffers', memory.buffers_bytes);
  setSeg('stat-mem-seg-cached', memory.cached_bytes);

  // Network
  setText('stat-net-iface', network.interface || '');
  setText('stat-net-rx', fmtBitRate(network.rx_bps));
  setText('stat-net-tx', fmtBitRate(network.tx_bps));

  // Load avg + uptime
  if (loadavg && loadavg.length === 3) {
    setText(
      'stat-load-value',
      `${loadavg[0].toFixed(2)} / ${loadavg[1].toFixed(2)} / ${loadavg[2].toFixed(2)}`,
    );
  }
  setText('dash-uptime', uptime_secs ? fmtDuration(uptime_secs) : '');
}

// Codecs that reach OME but not every viewer. AV1 is the only one worth
// flagging here: Safari has no software decoder for it (M3-or-later hardware
// only), so it plays fine for the operator and black for most of the room.
//
// H.265 deliberately isn't flagged. It plays in Chrome and Safari, which is the
// normal case for this product, so warning on every H.265 ingest was crying
// wolf — and the viewer now pre-flights codec support per browser and says so
// itself (`findUnplayableCodec` in frontend/viewer/player.ts), which is both
// more accurate and shown to the person actually affected.
function browserCodecWarning(codec: string): string {
  const c = codec.toLowerCase();
  if (c.includes('av1')) {
    return 'AV1 ingest — Safari needs M3-or-later hardware to decode this; most Macs will show no video.';
  }
  return '';
}

function renderOme(): void {
  const container = document.getElementById('ome-list');
  const subheader = document.getElementById('ome-subheader');
  if (!container || !subheader) return;

  if (!omeData) {
    container.innerHTML = '<div class="empty">Loading…</div>';
    return;
  }
  if (omeData.error) {
    container.innerHTML = `<div class="empty">OME unavailable — ${esc(omeData.error)}</div>`;
    subheader.textContent = '';
    return;
  }

  const { streams, conf_count } = omeData;
  subheader.textContent =
    conf_count > 0 ? `${conf_count} conference stream${conf_count !== 1 ? 's' : ''} active` : '';

  if (!streams.length) {
    container.innerHTML = '<div class="empty">No streams currently live.</div>';
    return;
  }

  container.innerHTML = streams
    .map((s) => {
      const input = s.detail?.input || {};
      const tracks = input.tracks || [];
      const vTrack = tracks.find((t) => t.type === 'Video');
      const aTrack = tracks.find((t) => t.type === 'Audio');
      const v = vTrack?.video;
      const a = aTrack?.audio;

      const videoStr = v
        ? `${v.codec} · ${v.width}×${v.height} · ${Math.round(v.framerate)}fps · ${fmtBitrate(v.bitrateLatest)}`
        : '—';
      // Farbstrom passes video through untouched, so the ingest codec is what
      // every viewer's browser has to decode. Two of them can't be relied on:
      // Safari ships no AV1 software decoder (needs M3-or-later hardware), and
      // HEVC decoding is hardware-gated nearly everywhere. Flag it here so an
      // operator finds out before the room does. (The viewer detects the same
      // condition on its own for LL-HLS and says so instead of showing black.)
      const codecWarning = v ? browserCodecWarning(v.codec) : '';
      const audioStr = a
        ? `${a.codec} · ${Math.round(a.samplerate / 1000)}kHz · ${a.channel}ch · ${fmtBitrate(a.bitrateLatest)}`
        : '—';

      const sourceType = input.sourceType || '?';
      const sourceIp = fmtSourceIp(input.sourceUrl);
      const uptime = input.createdTime ? fmtUptime(input.createdTime) : '';
      const meta = [uptime ? `Live ${uptime}` : '', sourceIp ? `from ${esc(sourceIp)}` : '']
        .filter(Boolean)
        .join(' · ');

      const displayName = s.key_name || (s.name.length > 16 ? s.name.slice(0, 16) + '…' : s.name);

      return `
      <div class="stream-card">
        <div class="stream-card-header">
          <div class="stream-live-dot"></div>
          <div class="stream-card-info">
            <div class="stream-card-name">${esc(displayName)}</div>
            <div class="stream-card-room">${
              s.room_name
                ? `Room: ${esc(s.room_name)}`
                : '<span style="color:var(--faint)">No room assigned</span>'
            }</div>
          </div>
          <span class="badge badge-source">${esc(sourceType)}</span>
          <button class="btn btn-sm" data-action="preview-stream" data-name="${esc(s.name)}" data-label="${esc(s.key_name || s.room_name || displayName)}">Preview</button>
          <button class="btn btn-sm btn-danger" data-action="kick-stream" data-name="${esc(s.name)}" data-label="${esc(s.key_name || s.room_name || displayName)}">Kick</button>
        </div>
        <div class="stream-card-body">
          <div class="stream-stat"><span class="stat-label">Video</span><span>${videoStr}</span></div>
          <div class="stream-stat"><span class="stat-label">Audio</span><span>${audioStr}</span></div>
        </div>
        ${codecWarning ? `<div class="stream-meta" style="color:var(--danger)">${esc(codecWarning)}</div>` : ''}
        ${meta ? `<div class="stream-meta">${meta}</div>` : ''}
      </div>`;
    })
    .join('');
}

// Live-stream preview lightbox (issue #204). The OME stream name is the ingest
// key token (OutputStreamName=${OriginStreamName}), so it doubles as the
// playback path — same URL shape the streamkeys tab and viewer use. `label` is
// the human-friendly key/room name for the title (never the raw token). WebRTC
// first with an LLHLS fallback so it plays regardless of the room's mode.
function openStreamPreview(name: string, label: string): void {
  const host = location.host;
  const proto = location.protocol === 'https:' ? 'https' : 'http';
  const wsproto = location.protocol === 'https:' ? 'wss' : 'ws';
  closeStreamPreview();
  openModal('stream-preview-modal');
  const titleEl = document.getElementById('stream-preview-title');
  if (titleEl) titleEl.textContent = label ? `Stream Preview — ${label}` : 'Stream Preview';
  previewPlayer = OvenPlayer.create('stream-preview-player', {
    autoStart: true,
    autoFallback: true,
    mute: true,
    sources: [
      { type: 'webrtc', file: `${wsproto}://${host}/live/${name}` },
      { type: 'll-hls', file: `${proto}://${host}/live/${name}/llhls.m3u8` },
    ],
    webrtcConfig: { timeoutMaxRetry: 3, connectionTimeout: 8000 },
    hlsConfig: { liveSyncDuration: 1, liveMaxLatencyDuration: 2, maxLiveSyncPlaybackRate: 1 },
  });
}

function closeStreamPreview(): void {
  if (previewPlayer) {
    try {
      previewPlayer.remove();
    } catch {
      /* player already torn down */
    }
    previewPlayer = null;
  }
  const container = document.getElementById('stream-preview-player');
  if (container) container.innerHTML = '';
  closeModal('stream-preview-modal');
}

async function kickStream(name: string, label: string): Promise<void> {
  if (
    !(await confirmModal({
      title: 'Kick Stream',
      message: `Kick stream "${label || name}"?\nThis disconnects the encoder and blocks the key from reconnecting. Re-enable it later with "Allow ingest" in the Streamkeys tab.`,
      confirmLabel: 'Kick',
      danger: true,
    }))
  )
    return;
  const res = await apiFetch(`/api/ome/streams/${encodeURIComponent(name)}`, { method: 'DELETE' });
  if (res && res.ok) {
    toast('Stream kicked');
    void loadOme();
    onStreamKicked();
  } else {
    toast('Kick failed');
  }
}

export function renderOmeIfReady(): void {
  if (omeData) renderOme();
}

export function initDashboard(): void {
  document.getElementById('ome-refresh-btn')?.addEventListener('click', () => void loadOme());
  document
    .getElementById('stream-preview-close')
    ?.addEventListener('click', closeStreamPreview);
  document
    .getElementById('stream-preview-dismiss')
    ?.addEventListener('click', closeStreamPreview);
}

export function handleDashboardAction(action: string, target: HTMLElement): void {
  const name = target.getAttribute('data-name') || '';
  const label = target.getAttribute('data-label') || '';
  if (action === 'kick-stream') {
    void kickStream(name, label);
  } else if (action === 'preview-stream') {
    openStreamPreview(name, label);
  }
}
