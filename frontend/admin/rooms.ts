import { apiFetch } from './auth.js';
import { closeModal, confirmModal, openModal } from '../shared/components.js';
import { copyToClipboard, esc, fmtDateTime, toast } from '../shared/utils.js';
import type { EnterRoomResponse, Participant, Room, StreamKey } from './types.js';

const VIEWER_BASE = `${location.origin}/watch`;

let rooms: Room[] = [];
let keys: StreamKey[] = [];
let waitingMap: Record<string, Participant[]> = {};
let kickedMap: Record<string, Participant[]> = {};
let editingRoomId: string | null = null;
let enterRoomId: string | null = null;
let lastListsSnap = '';

let onChange: () => void = () => {};

export function getRooms(): Room[] {
  return rooms;
}

export function setStreamKeys(next: StreamKey[]): void {
  keys = next;
}

export function setOnChange(fn: () => void): void {
  onChange = fn;
}

async function fetchParticipantLists(): Promise<{
  waiting: Record<string, Participant[]>;
  kicked: Record<string, Participant[]>;
}> {
  const snap = rooms.slice();
  const withWaiting = snap.filter((r) => r.waiting_room && r.status !== 'ended');
  const activeRooms = snap.filter((r) => r.status !== 'ended');
  const fetchList = async (path: string): Promise<Participant[]> => {
    const r = await apiFetch(path);
    if (!r || !r.ok) return [];
    return r.json().catch(() => []);
  };
  const [waitingResults, kickedResults] = await Promise.all([
    Promise.all(
      withWaiting.map(async (r) => [r.id, await fetchList(`/api/rooms/${r.id}/waiting`)] as const),
    ),
    Promise.all(
      activeRooms.map(async (r) => [r.id, await fetchList(`/api/rooms/${r.id}/kicked`)] as const),
    ),
  ]);
  return {
    waiting: Object.fromEntries(waitingResults),
    kicked: Object.fromEntries(kickedResults),
  };
}

export async function loadRooms(): Promise<void> {
  const res = await apiFetch('/api/rooms');
  if (!res) return;
  rooms = await res.json();
  const lists = await fetchParticipantLists();
  waitingMap = lists.waiting;
  kickedMap = lists.kicked;
  lastListsSnap = JSON.stringify([waitingMap, kickedMap]);
  renderRooms();
}

// Fast poll for waiting/kicked lists so the admin sees join requests and
// expulsions within seconds. Re-renders only when the lists actually
// changed to avoid disrupting hover/focus state.
export async function refreshParticipantLists(): Promise<void> {
  if (!rooms.length) return;
  const lists = await fetchParticipantLists();
  const snap = JSON.stringify([lists.waiting, lists.kicked]);
  if (snap === lastListsSnap) return;
  waitingMap = lists.waiting;
  kickedMap = lists.kicked;
  lastListsSnap = snap;
  renderRooms();
}

function renderRooms(): void {
  const container = document.getElementById('rooms-list');
  if (!container) return;
  if (!rooms.length) {
    container.innerHTML = '<div class="empty">No rooms yet. Create one to get started.</div>';
    return;
  }

  container.innerHTML = rooms
    .map((r) => {
      const viewerUrl = `${VIEWER_BASE}/${r.slug}`;
      const hostUrl = `${VIEWER_BASE}/${r.slug}#role=presenter&pk=${r.presenter_key}`;
      const expires = fmtDateTime(r.expires_at);
      const keyLabel = r.stream_key_name || '—';

      const metaParts = [`/${r.slug}`];
      if (keyLabel !== '—') metaParts.push(`Key: ${esc(keyLabel)}`);
      if (expires) metaParts.push(`Expires ${expires}`);

      const waitingPeople = waitingMap[r.id] || [];
      const waitingCount = waitingPeople.length;
      const showWaiting = !!r.waiting_room && r.status !== 'ended';
      const kicked = kickedMap[r.id] || [];

      return `
      <div class="room-card" data-id="${esc(r.id)}" data-slug="${esc(r.slug)}">
        <div class="room-card-header">
          <div class="room-card-info">
            <div class="room-card-name">${esc(r.name)}</div>
            <div class="room-card-meta">${metaParts.join(' · ')}</div>
          </div>
          <div class="room-card-badges">
            <span class="badge badge-${esc(r.status)}">${esc(r.status)}</span>
            <span class="badge badge-${esc(r.delivery_mode)}">${esc(r.delivery_mode)}</span>
          </div>
          <div class="room-card-actions">
            ${
              r.status === 'ended'
                ? `<button class="btn btn-sm" data-action="reactivate-room" data-id="${esc(r.id)}">Reactivate</button>`
                : ''
            }
            <button class="btn btn-sm" data-action="edit-room" data-id="${esc(r.id)}">Edit</button>
            <button class="btn btn-sm btn-danger" data-action="delete-room" data-id="${esc(r.id)}">Delete</button>
          </div>
        </div>

        <div class="room-card-body">
          <div class="url-row">
            <span class="url-label">Viewer</span>
            <input readonly class="url-input" value="${esc(viewerUrl)}">
          </div>
          <div class="url-row host-actions">
            <span class="url-label">Host</span>
            <button class="btn btn-sm btn-primary" data-action="enter-presenter" data-id="${esc(r.id)}">Enter Room</button>
            <button class="btn btn-sm" data-action="copy" data-value="${esc(hostUrl)}" title="Copy a host link to share with a colorist">Share with host</button>
            <button class="btn btn-sm" data-action="rotate-host-key" data-id="${esc(r.id)}" title="Invalidate the current host link">Rotate</button>
          </div>
        </div>

        ${
          showWaiting
            ? `
        <div class="waiting-section">
          <div class="waiting-section-header">
            <span class="waiting-section-title">Waiting Room${waitingCount > 0 ? ` (${waitingCount})` : ''}</span>
            ${waitingCount > 0 ? `<button class="btn btn-sm" data-action="admit-all" data-id="${esc(r.id)}">Admit All</button>` : ''}
          </div>
          ${
            waitingPeople.length
              ? waitingPeople
                  .map(
                    (p) => `
              <div class="participant-row">
                <span>${esc(p.name)}</span>
                <button class="btn btn-sm" data-action="admit-one" data-room="${esc(r.id)}" data-pid="${esc(p.id)}">Admit</button>
              </div>`,
                  )
                  .join('')
              : `<span class="waiting-empty">No one waiting.</span>`
          }
        </div>`
            : ''
        }

        ${
          kicked.length
            ? `
        <div class="waiting-section">
          <div class="waiting-section-header">
            <span class="waiting-section-title" style="color:var(--danger)">Kicked (${kicked.length})</span>
          </div>
          ${kicked
            .map(
              (p) => `
            <div class="participant-row">
              <span>${esc(p.name)}</span>
              <button class="btn btn-sm" data-action="unkick-one" data-room="${esc(r.id)}" data-pid="${esc(p.id)}">Unblock</button>
            </div>`,
            )
            .join('')}
        </div>`
            : ''
        }
      </div>`;
    })
    .join('');
}

// ---- Room modal ----

function openRoomModal(id: string | null): void {
  editingRoomId = id;
  const titleEl = document.getElementById('room-modal-title');
  if (titleEl) titleEl.textContent = id ? 'Edit Room' : 'New Room';

  const skSelect = document.getElementById('room-stream-key') as HTMLSelectElement | null;
  if (skSelect) {
    skSelect.innerHTML =
      '<option value="">— None —</option>' +
      keys.map((k) => `<option value="${esc(k.id)}">${esc(k.name)}</option>`).join('');
  }

  const clearPwd = document.getElementById('room-clear-password') as HTMLInputElement | null;
  if (clearPwd) clearPwd.checked = false;

  if (id) {
    const r = rooms.find((x) => x.id === id);
    if (!r) return;
    (document.getElementById('room-name') as HTMLInputElement).value = r.name;
    (document.getElementById('room-password') as HTMLInputElement).value = '';
    (document.getElementById('room-delivery') as HTMLSelectElement).value = r.delivery_mode;
    (document.getElementById('room-waiting') as HTMLInputElement).checked = !!r.waiting_room;
    (document.getElementById('room-noise-reduction') as HTMLInputElement).checked =
      !!r.noise_reduction;
    (document.getElementById('room-echo-cancellation') as HTMLInputElement).checked =
      !!r.echo_cancellation;
    (document.getElementById('room-push-to-talk') as HTMLInputElement).checked = !!r.push_to_talk;
    const expiresInput = document.getElementById('room-expires') as HTMLInputElement;
    if (r.expires_at) {
      // expires_at is stored as UTC "YYYY-MM-DD HH:MM:SS" with no zone marker.
      // Force UTC parse, then format the local-time wall clock for datetime-local.
      const d = new Date(r.expires_at.replace(' ', 'T') + 'Z');
      const local = new Date(d.getTime() - d.getTimezoneOffset() * 60000);
      expiresInput.value = local.toISOString().slice(0, 16);
    } else {
      expiresInput.value = '';
    }
    if (skSelect) skSelect.value = r.stream_key_id || '';
    const clearRow = document.getElementById('clear-password-row');
    if (clearRow) clearRow.style.display = r.password_hash ? '' : 'none';
  } else {
    (document.getElementById('room-name') as HTMLInputElement).value = '';
    (document.getElementById('room-password') as HTMLInputElement).value = '';
    (document.getElementById('room-delivery') as HTMLSelectElement).value = 'webrtc';
    (document.getElementById('room-waiting') as HTMLInputElement).checked = false;
    (document.getElementById('room-noise-reduction') as HTMLInputElement).checked = true;
    (document.getElementById('room-echo-cancellation') as HTMLInputElement).checked = true;
    (document.getElementById('room-push-to-talk') as HTMLInputElement).checked = false;
    (document.getElementById('room-expires') as HTMLInputElement).value = '';
    if (skSelect) skSelect.value = '';
    const clearRow = document.getElementById('clear-password-row');
    if (clearRow) clearRow.style.display = 'none';
  }

  openModal('room-modal');
}

function closeRoomModal(): void {
  closeModal('room-modal');
  const clearRow = document.getElementById('clear-password-row');
  if (clearRow) clearRow.style.display = 'none';
  const clearPwd = document.getElementById('room-clear-password') as HTMLInputElement | null;
  if (clearPwd) clearPwd.checked = false;
  editingRoomId = null;
}

async function saveRoom(): Promise<void> {
  const name = (document.getElementById('room-name') as HTMLInputElement).value.trim();
  const password = (document.getElementById('room-password') as HTMLInputElement).value;
  const clearPassword = (document.getElementById('room-clear-password') as HTMLInputElement)
    .checked;
  const delivery_mode = (document.getElementById('room-delivery') as HTMLSelectElement).value;
  const waiting_room = (document.getElementById('room-waiting') as HTMLInputElement).checked;
  const noise_reduction = (document.getElementById('room-noise-reduction') as HTMLInputElement)
    .checked;
  const echo_cancellation = (document.getElementById('room-echo-cancellation') as HTMLInputElement)
    .checked;
  const push_to_talk = (document.getElementById('room-push-to-talk') as HTMLInputElement).checked;
  const expiresRaw = (document.getElementById('room-expires') as HTMLInputElement).value;
  const expires_at = expiresRaw
    ? new Date(expiresRaw).toISOString().replace('T', ' ').replace(/\.\d{3}Z$/, '')
    : null;
  const streamKeyId = (document.getElementById('room-stream-key') as HTMLSelectElement).value;

  if (!name) {
    toast('Room name required');
    return;
  }

  // Password logic:
  //   clearPassword checked → send '' to clear
  //   password typed        → send new value to hash
  //   neither               → omit field (backend keeps existing hash)
  const body: Record<string, unknown> = {
    name,
    delivery_mode,
    waiting_room,
    noise_reduction,
    echo_cancellation,
    push_to_talk,
    expires_at,
    stream_key_id: streamKeyId || null,
    ...(clearPassword ? { password: '' } : password ? { password } : {}),
  };

  const res = editingRoomId
    ? await apiFetch(`/api/rooms/${editingRoomId}`, { method: 'PUT', body: JSON.stringify(body) })
    : await apiFetch('/api/rooms', { method: 'POST', body: JSON.stringify(body) });

  if (!res || !res.ok) {
    toast('Save failed');
    return;
  }
  closeRoomModal();
  toast(editingRoomId ? 'Room updated' : 'Room created');
  onChange();
}

// ---- Enter as presenter modal ----

function openEnterModal(id: string): void {
  enterRoomId = id;
  const nameEl = document.getElementById('enter-name') as HTMLInputElement;
  nameEl.value = 'Host';
  openModal('enter-modal');
  nameEl.focus();
  nameEl.select();
}

async function doEnterRoom(): Promise<void> {
  if (!enterRoomId) return;
  const name = (document.getElementById('enter-name') as HTMLInputElement).value.trim() || 'Host';
  closeModal('enter-modal');
  const res = await apiFetch(`/api/rooms/${enterRoomId}/enter`, {
    method: 'POST',
    body: JSON.stringify({ name }),
  });
  if (!res || !res.ok) {
    toast('Failed to enter room');
    return;
  }
  const data: EnterRoomResponse = await res.json();
  localStorage.setItem(
    `_presession_${data.slug}`,
    JSON.stringify({
      participantId: data.participantId,
      token: data.token,
      deliveryMode: data.deliveryMode,
      streamKey: data.streamKey,
      role: 'presenter',
    }),
  );
  window.open(`/watch/${data.slug}`, '_blank');
}

// ---- Public action handlers (invoked by main.ts dispatcher) ----

export async function deleteRoom(id: string): Promise<void> {
  if (
    !(await confirmModal({
      title: 'Delete Room',
      message: 'This room and its participants will be permanently removed.',
      confirmLabel: 'Delete',
      danger: true,
    }))
  )
    return;
  const res = await apiFetch(`/api/rooms/${id}`, { method: 'DELETE' });
  if (res && res.ok) {
    toast('Room deleted');
    onChange();
  } else {
    toast('Delete failed');
  }
}

export async function reactivateRoom(id: string): Promise<void> {
  const res = await apiFetch(`/api/rooms/${id}/reactivate`, { method: 'POST' });
  if (res && res.ok) {
    toast('Room reactivated');
    onChange();
  } else {
    toast('Reactivate failed');
  }
}

export async function admitOne(roomId: string, participantId: string): Promise<void> {
  await apiFetch(`/api/rooms/${roomId}/admit/${participantId}`, { method: 'POST' });
  loadRooms();
}

export async function admitAll(roomId: string): Promise<void> {
  await apiFetch(`/api/rooms/${roomId}/admit-all`, { method: 'POST' });
  toast('All admitted');
  loadRooms();
}

export async function rotateHostKey(id: string): Promise<void> {
  if (
    !(await confirmModal({
      title: 'Rotate host link?',
      message:
        'A new host link will be generated. The current link will stop granting host privileges.',
      confirmLabel: 'Rotate',
      danger: true,
    }))
  )
    return;
  const res = await apiFetch(`/api/rooms/${id}/rotate-presenter-key`, { method: 'POST' });
  if (res && res.ok) {
    const updated = (await res.json()) as Room;
    const idx = rooms.findIndex((x) => x.id === id);
    if (idx >= 0) rooms[idx] = updated;
    toast('Host link rotated');
    renderRooms();
  } else {
    toast('Rotate failed');
  }
}

export async function unkickOne(roomId: string, participantId: string): Promise<void> {
  await apiFetch(`/api/rooms/${roomId}/unkick/${participantId}`, { method: 'POST' });
  toast('Participant unblocked');
  loadRooms();
}

// ---- Wire DOM ----

export function initRooms(): void {
  document.getElementById('new-room-btn')?.addEventListener('click', () => openRoomModal(null));
  document.getElementById('room-modal-close')?.addEventListener('click', closeRoomModal);
  document.getElementById('room-modal-cancel')?.addEventListener('click', closeRoomModal);
  document.getElementById('room-modal-save')?.addEventListener('click', saveRoom);

  document.getElementById('enter-modal-go')?.addEventListener('click', doEnterRoom);
  document
    .getElementById('enter-modal-cancel')
    ?.addEventListener('click', () => closeModal('enter-modal'));
  document
    .getElementById('enter-modal-close')
    ?.addEventListener('click', () => closeModal('enter-modal'));
  document.getElementById('enter-name')?.addEventListener('keydown', (e) => {
    if ((e as KeyboardEvent).key === 'Enter') doEnterRoom();
  });
}

// Dispatch from main.ts delegated handler
export function handleRoomAction(action: string, target: HTMLElement): void {
  const id = target.getAttribute('data-id') || '';
  switch (action) {
    case 'edit-room':
      openRoomModal(id);
      break;
    case 'delete-room':
      void deleteRoom(id);
      break;
    case 'reactivate-room':
      void reactivateRoom(id);
      break;
    case 'enter-presenter':
      openEnterModal(id);
      break;
    case 'rotate-host-key':
      void rotateHostKey(id);
      break;
    case 'admit-all':
      void admitAll(id);
      break;
    case 'admit-one': {
      const room = target.getAttribute('data-room') || '';
      const pid = target.getAttribute('data-pid') || '';
      void admitOne(room, pid);
      break;
    }
    case 'unkick-one': {
      const room = target.getAttribute('data-room') || '';
      const pid = target.getAttribute('data-pid') || '';
      void unkickOne(room, pid);
      break;
    }
    case 'copy': {
      const v = target.getAttribute('data-value') || '';
      copyToClipboard(v);
      break;
    }
  }
}
