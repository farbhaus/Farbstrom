// In-room roster tab: shows everyone present (all roles) plus, for
// presenters, the waiting + kicked lists with admit/kick/unkick controls.
//
// Waiting + kicked data arrives via the `moderation:update` WS message
// (server-side filtered to presenters only, so non-hosts never see those
// lists even though the markup is in the same panel).

import { toast } from '../shared/utils.js';
import { getParticipantId, getToken, slug } from './session.js';
import { viewerStore } from './state.js';

interface ModParticipant {
  id: string;
  name: string;
}

let waiting: ModParticipant[] = [];
let kicked: ModParticipant[] = [];
// Admitted, non-kicked participants (presenter-only). We render the ones with no
// live WS presence — Farbplay builds predating pointer collaboration, which open
// no WS at all — so the host can see and kick them. Current Farbplay builds are
// identified by their `client` marker on the WS roster instead (gh #227).
let admitted: ModParticipant[] = [];

function esc(s: string): string {
  return s.replace(/[&<>"']/g, (c) =>
    c === '&' ? '&amp;' : c === '<' ? '&lt;' : c === '>' ? '&gt;' : c === '"' ? '&quot;' : '&#39;',
  );
}

function isPresenter(): boolean {
  return viewerStore.get().role === 'presenter';
}

// One in-room roster row. `roleLabel` is shown verbatim (e.g. 'viewer', 'SRT').
function rosterRow(id: string, name: string, roleLabel: string): string {
  return `
        <div class="roster-row" data-id="${esc(id)}">
          <span class="roster-name">${esc(name)}</span>
          <span class="roster-role">${esc(roleLabel)}</span>
          ${isPresenter() ? `<button class="btn-mini danger" data-action="roster-kick" data-id="${esc(id)}">Kick</button>` : ''}
        </div>`;
}

export function applyHostMode(): void {
  document.body.classList.toggle('has-host', isPresenter());
}

export function renderRoster(): void {
  const { roster } = viewerStore.get();
  const self = getParticipantId();
  // Farbplay holds a WS of its own (pointer collaboration), so it lands in the
  // roster like any browser viewer — its `client` marker is what keeps it out of
  // the in-room list and in the Farbplay section below (gh #227).
  const inRoom = roster.filter((p) => p.id !== self && p.client !== 'farbplay');
  const fpInRoster = roster.filter((p) => p.id !== self && p.client === 'farbplay');

  const inEl = document.getElementById('roster-inroom');
  const inCount = document.getElementById('roster-inroom-count');
  if (inEl && inCount) {
    inCount.textContent = String(inRoom.length);
    inEl.innerHTML =
      inRoom.length === 0
        ? `<div class="roster-empty">Just you for now.</div>`
        : inRoom.map((p) => rosterRow(p.id, p.name, p.role)).join('');
  }

  // Connected Farbplay (SRT) viewers, from two sources: the WS roster's `client`
  // marker, plus admitted participants with SSE presence but no WS at all —
  // Farbplay builds predating pointer collaboration. The two sets are disjoint by
  // construction (a marked viewer has WS presence, so it can't be in the second),
  // so the union needs no dedupe. Shown to everyone in a dedicated section so the
  // roster matches the participant count; the Kick control is host-only. The
  // whole section is hidden when there are no app viewers.
  const presentIds = new Set(roster.map((p) => p.id));
  const legacy = admitted.filter((p) => p.id !== self && !presentIds.has(p.id));
  const srtViewers = [...fpInRoster.map((p) => ({ id: p.id, name: p.name })), ...legacy];
  const sSection = document.getElementById('roster-srt-section');
  const sEl = document.getElementById('roster-srt');
  const sCount = document.getElementById('roster-srt-count');
  if (sSection && sEl && sCount) {
    sSection.style.display = srtViewers.length === 0 ? 'none' : '';
    sCount.textContent = String(srtViewers.length);
    sEl.innerHTML = srtViewers
      .map(
        (p) => `
        <div class="roster-row" data-id="${esc(p.id)}">
          <span class="roster-name">${esc(p.name)}</span>
          ${isPresenter() ? `<button class="btn-mini danger" data-action="roster-kick" data-id="${esc(p.id)}">Kick</button>` : ''}
        </div>`,
      )
      .join('');
  }

  // Participant-count badge: browser participants plus connected Farbplay
  // viewers, so the button matches what the roster box shows. Marked viewers are
  // subtracted out of `roster` first — they're counted again in srtViewers.
  const numEl = document.getElementById('participant-num');
  if (numEl) {
    numEl.textContent = String(roster.length - fpInRoster.length + srtViewers.length);
  }

  const wEl = document.getElementById('roster-waiting');
  const wCount = document.getElementById('roster-waiting-count');
  const admitAllBtn = document.getElementById('roster-admit-all');
  if (wEl && wCount) {
    wCount.textContent = String(waiting.length);
    if (admitAllBtn) admitAllBtn.style.display = waiting.length > 1 ? '' : 'none';
    wEl.innerHTML =
      waiting.length === 0
        ? `<div class="roster-empty">Nobody waiting.</div>`
        : waiting
            .map(
              (p) => `
        <div class="roster-row" data-id="${esc(p.id)}">
          <span class="roster-name">${esc(p.name)}</span>
          <button class="btn-mini primary" data-action="roster-admit" data-id="${esc(p.id)}">Admit</button>
        </div>`,
            )
            .join('');
  }

  const kEl = document.getElementById('roster-kicked');
  const kCount = document.getElementById('roster-kicked-count');
  if (kEl && kCount) {
    kCount.textContent = String(kicked.length);
    kEl.innerHTML =
      kicked.length === 0
        ? `<div class="roster-empty">Nobody kicked.</div>`
        : kicked
            .map(
              (p) => `
        <div class="roster-row" data-id="${esc(p.id)}">
          <span class="roster-name">${esc(p.name)}</span>
          <button class="btn-mini" data-action="roster-unkick" data-id="${esc(p.id)}">Unblock</button>
        </div>`,
            )
            .join('');
  }
}

export function applyModerationUpdate(next: {
  waiting: ModParticipant[];
  kicked: ModParticipant[];
  admitted: ModParticipant[];
  newWaiting: string[];
}): void {
  waiting = next.waiting;
  kicked = next.kicked;
  admitted = next.admitted;
  renderRoster();

  // Toast on new arrivals (presenter-only by construction — viewers never
  // receive the message because the server filters).
  for (const name of next.newWaiting) {
    toast(`${name} is waiting…`);
  }

  // Light the notification dot on the participant count when new waiting
  // arrives and the user isn't already looking at the roster.
  if (next.newWaiting.length > 0 && !isRosterOpen()) {
    document.getElementById('roster-dot')?.classList.add('visible');
  }
}

function isRosterOpen(): boolean {
  return !document.getElementById('roster-overlay')?.classList.contains('hidden');
}

function openRoster(): void {
  document.getElementById('roster-overlay')?.classList.remove('hidden');
  document.getElementById('roster-dot')?.classList.remove('visible');
}

function closeRoster(): void {
  document.getElementById('roster-overlay')?.classList.add('hidden');
}

async function api(path: string, body: Record<string, unknown>): Promise<boolean> {
  try {
    const res = await fetch(`/api/public/rooms/${slug}/conference/${path}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        participantId: getParticipantId(),
        token: getToken(),
        ...body,
      }),
    });
    if (!res.ok) {
      toast('Action failed');
      return false;
    }
    return true;
  } catch {
    toast('Action failed');
    return false;
  }
}

export function initRoster(): void {
  document.getElementById('participant-count')?.addEventListener('click', () => {
    if (isRosterOpen()) closeRoster();
    else openRoster();
  });
  document.getElementById('roster-close')?.addEventListener('click', closeRoster);
  document.getElementById('roster-overlay')?.addEventListener('click', (e) => {
    if ((e.target as HTMLElement).id === 'roster-overlay') closeRoster();
  });
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && isRosterOpen()) closeRoster();
  });

  document.getElementById('roster-admit-all')?.addEventListener('click', () => {
    void api('admit-all', {});
  });

  document.getElementById('roster-body')?.addEventListener('click', (e) => {
    const btn = (e.target as HTMLElement).closest<HTMLElement>('[data-action]');
    if (!btn) return;
    const action = btn.dataset['action'];
    const id = btn.dataset['id'] || '';
    if (!id) return;
    if (action === 'roster-admit') void api(`admit/${id}`, {});
    else if (action === 'roster-unkick') void api(`unkick/${id}`, {});
    else if (action === 'roster-kick') void api('kick', { targetId: id });
  });

  // Re-render when the roster or role changes.
  viewerStore.subscribe(() => {
    applyHostMode();
    renderRoster();
  });

  applyHostMode();
  renderRoster();
}
