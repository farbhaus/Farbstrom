// Chat sidebar + file sharing (upload XHR with cancel + progress).

import { esc, fmtBytes } from '../shared/utils.js';
import { getParticipantId, getToken, slug } from './session.js';
import { viewerStore } from './state.js';
import type { Role, SessionFile, WsClientMessage } from './types.js';

let sendFn: ((msg: WsClientMessage) => void) | null = null;

export function configureChat(opts: { send: (msg: WsClientMessage) => void }): void {
  sendFn = opts.send;
}

export function setChatEnabled(enabled: boolean): void {
  (document.getElementById('chat-input') as HTMLTextAreaElement).disabled = !enabled;
  (document.getElementById('chat-attach') as HTMLButtonElement).disabled = !enabled;
  // chat-send tracks input+draft state; let syncSendButton make the call.
  syncSendButton();
}

function notifyChat(): void {
  if (!viewerStore.get().chatOpen) {
    document.getElementById('chat-toggle')?.classList.add('has-notification');
  }
}

function fmtTime(ts: number): string {
  const t = new Date(ts);
  return (
    t.getHours().toString().padStart(2, '0') + ':' + t.getMinutes().toString().padStart(2, '0')
  );
}

function dlUrl(fileId: string): string {
  return (
    `/api/public/rooms/${encodeURIComponent(slug)}/files/${encodeURIComponent(fileId)}/download` +
    `?participantId=${encodeURIComponent(getParticipantId())}&token=${encodeURIComponent(getToken())}`
  );
}

// Files we can offer to display in the unified stage. .mov is included
// because most are H.264 in QuickTime container — the backend's display
// route relabels them as video/mp4 so Chrome / Firefox will play them.
// ProRes / DNxHD MOVs will still fail at decode time; player.ts handles
// that error by clearing display state and toasting the presenter.
const PLAYABLE_VIDEO_MIMES = new Set(['video/mp4', 'video/webm', 'video/quicktime']);
function canShow(mime: string | undefined): boolean {
  if (!mime) return false;
  const m = mime.toLowerCase();
  return m.startsWith('image/') || PLAYABLE_VIDEO_MIMES.has(m);
}

function showBtnHtml(fileId: string, klass: string): string {
  const current = viewerStore.get().displayFile?.fileId === fileId;
  const label = current ? 'Hide' : 'Show';
  const cls = klass + (current ? ' is-active' : '');
  return (
    `<button class="${cls}" data-action="display-show" ` +
    `data-file-id="${esc(fileId)}" title="${current ? 'Stop showing in room' : 'Show in room'}">${label}</button>`
  );
}

// Delete button — presenter-only, removes the file from this room
// (broadcast via file:removed so every client drops it from chat + the
// files panel).
function deleteBtnHtml(fileId: string, klass: string): string {
  return (
    `<button class="${klass}" data-action="file-delete" ` +
    `data-file-id="${esc(fileId)}" title="Remove from this room" aria-label="Remove from this room">` +
    `<svg viewBox="0 0 24 24" stroke="currentColor" fill="none" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><polyline points="3 6 5 6 21 6"/><path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"/><path d="M10 11v6"/><path d="M14 11v6"/><path d="M9 6V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2"/></svg>` +
    `</button>`
  );
}

// Walk every Show/Hide button in the chat column and refresh its label
// against the current `displayFile`. Called from a viewerStore subscriber.
export function refreshShowButtons(): void {
  const currentId = viewerStore.get().displayFile?.fileId || null;
  const buttons = document.querySelectorAll<HTMLButtonElement>('[data-action="display-show"]');
  buttons.forEach((btn) => {
    const isThis = btn.dataset['fileId'] === currentId && !!currentId;
    btn.textContent = isThis ? 'Hide' : 'Show';
    btn.title = isThis ? 'Stop showing in room' : 'Show in room';
    btn.classList.toggle('is-active', isThis);
  });
}

interface ChatMsg {
  ts: number;
  name: string;
  role: Role;
  text: string;
}

export function appendChatMessage(msg: ChatMsg): void {
  const list = document.getElementById('chat-messages');
  if (!list) return;
  const d = document.createElement('div');
  d.className = 'chat-msg';
  d.innerHTML =
    `<div class="chat-meta"><span class="chat-who ${esc(msg.role)}">${esc(msg.name)}</span><span class="chat-time">${fmtTime(msg.ts)}</span></div>` +
    `<div class="chat-text">${esc(msg.text)}</div>`;
  list.appendChild(d);
  list.scrollTop = list.scrollHeight;
  notifyChat();
}

interface FileMsg {
  ts: number;
  name: string;
  role: Role;
  id: string;
  size: number;
  mime?: string;
  uploaderName: string;
}

export function appendFileMessage(msg: FileMsg, notify = true): void {
  const list = document.getElementById('chat-messages');
  if (!list) return;
  const url = dlUrl(msg.id);
  const isPresenter = viewerStore.get().role === 'presenter';
  const showBtn = isPresenter && canShow(msg.mime) ? showBtnHtml(msg.id, 'file-row-show') : '';
  const delBtn = isPresenter ? deleteBtnHtml(msg.id, 'file-row-del') : '';
  const d = document.createElement('div');
  d.className = 'chat-msg';
  // The file element reuses the Files-tab `.file-row` look so a shared file
  // reads identically in both tabs. (The draft chip keeps `.chat-file`.)
  d.innerHTML =
    `<div class="chat-meta"><span class="chat-who ${esc(msg.role)}">${esc(msg.uploaderName)}</span><span class="chat-time">${fmtTime(msg.ts)}</span></div>` +
    `<div class="file-row" data-file-id="${esc(msg.id)}" data-mime="${esc(msg.mime || '')}">` +
    `<div class="file-row-name" title="${esc(msg.name)}">${esc(msg.name)}</div>` +
    `<span class="file-row-size">${fmtBytes(msg.size)}</span>` +
    showBtn +
    `<a class="file-row-dl" href="${url}" download="${esc(msg.name)}">Get</a>` +
    delBtn +
    `</div>`;
  list.appendChild(d);
  list.scrollTop = list.scrollHeight;
  if (notify) notifyChat();
}

export function addFileToSection(f: SessionFile, notify = true): void {
  const list = document.getElementById('files-list');
  if (!list) return;
  document.getElementById('files-empty')?.remove();
  if (list.querySelector(`[data-fid="${CSS.escape(f.id)}"]`)) return;
  const url = dlUrl(f.id);
  const isPresenter = viewerStore.get().role === 'presenter';
  const showBtn = isPresenter && canShow(f.mime) ? showBtnHtml(f.id, 'file-row-show') : '';
  const delBtn = isPresenter ? deleteBtnHtml(f.id, 'file-row-del') : '';
  const row = document.createElement('div');
  row.className = 'file-row';
  row.dataset['fid'] = f.id;
  if (f.mime) row.dataset['mime'] = f.mime;
  row.innerHTML =
    `<div class="file-row-name" title="${esc(f.name)}">${esc(f.name)}</div>` +
    `<span class="file-row-size">${fmtBytes(f.size)}</span>` +
    showBtn +
    `<a class="file-row-dl" href="${url}" download="${esc(f.name)}">Get</a>` +
    delBtn;
  list.appendChild(row);
  const count = document.getElementById('files-count');
  if (count) count.textContent = String(list.querySelectorAll('.file-row').length);
  // Dot the Files tab when a file arrives while another tab is showing, so the
  // (now hidden) list still signals new arrivals.
  if (notify) {
    const filesTab = document.getElementById('tab-files');
    if (filesTab && !filesTab.classList.contains('is-active')) {
      filesTab.classList.add('has-notification');
    }
  }
}

export function appendChatHistory(
  messages: Array<ChatMsg | (FileMsg & { type: 'file:shared' })>,
): void {
  for (const m of messages) {
    if ('type' in m && m.type === 'file:shared') {
      appendFileMessage(m as FileMsg, false);
      {
        const mime = (m as FileMsg).mime;
        addFileToSection(
          {
            id: m.id,
            name: m.name,
            size: m.size,
            ...(mime ? { mime } : {}),
            uploaderName: m.uploaderName,
            role: m.role,
          },
          false,
        );
      }
    } else {
      const list = document.getElementById('chat-messages');
      if (!list) continue;
      const d = document.createElement('div');
      d.className = 'chat-msg';
      d.innerHTML =
        `<div class="chat-meta"><span class="chat-who ${esc(m.role)}">${esc(m.name)}</span><span class="chat-time">${fmtTime(m.ts)}</span></div>` +
        `<div class="chat-text">${esc((m as ChatMsg).text)}</div>`;
      list.appendChild(d);
    }
  }
  const list = document.getElementById('chat-messages');
  if (list) list.scrollTop = list.scrollHeight;
}

export async function loadSessionFiles(): Promise<void> {
  try {
    const res = await fetch(
      `/api/public/rooms/${encodeURIComponent(slug)}/files` +
        `?participantId=${encodeURIComponent(getParticipantId())}&token=${encodeURIComponent(getToken())}`,
    );
    if (!res.ok) return;
    const files: SessionFile[] = await res.json();
    files.forEach((f) => addFileToSection(f, false));
  } catch {}
}

let currentUploadXhr: XMLHttpRequest | null = null;
// A file that has been uploaded (defer=true) and is waiting to be sent
// alongside a chat message. Cleared when the user sends or removes the chip.
let currentDraft: { id: string; name: string; size: number } | null = null;

function uploadFile(file: File): void {
  if (currentDraft) {
    // Only one draft attached at a time. Remove the existing one first.
    void clearDraft({ deleteRemote: true });
  }
  const attachBtn = document.getElementById('chat-attach') as HTMLButtonElement;
  const progressBar = document.getElementById('upload-progress-bar') as HTMLElement;
  const progressFill = document.getElementById('upload-progress-fill') as HTMLElement;
  const progressRow = document.getElementById('upload-progress-row') as HTMLElement;
  const progressName = document.getElementById('upload-progress-name') as HTMLElement;
  const progressPct = document.getElementById('upload-progress-pct') as HTMLElement;
  const cancelBtn = document.getElementById('upload-progress-cancel') as HTMLButtonElement;

  attachBtn.disabled = true;
  progressBar.style.display = '';
  progressRow.style.display = 'flex';
  progressFill.style.width = '0%';
  progressName.textContent = file.name;
  progressName.title = file.name;
  progressPct.textContent = '0%';

  const xhr = new XMLHttpRequest();
  currentUploadXhr = xhr;
  xhr.open(
    'POST',
    `/api/public/rooms/${encodeURIComponent(slug)}/files` +
      `?participantId=${encodeURIComponent(getParticipantId())}` +
      `&token=${encodeURIComponent(getToken())}&defer=true`,
  );
  xhr.upload.addEventListener('progress', (e) => {
    if (e.lengthComputable) {
      const pct = Math.round((e.loaded / e.total) * 100);
      progressFill.style.width = pct + '%';
      progressPct.textContent = pct + '%';
    }
  });
  const hideProgress = (): void => {
    currentUploadXhr = null;
    attachBtn.disabled = false;
    progressBar.style.display = 'none';
    progressRow.style.display = 'none';
    progressFill.style.width = '0%';
  };
  xhr.onload = () => {
    hideProgress();
    if (xhr.status >= 200 && xhr.status < 300) {
      try {
        const body = JSON.parse(xhr.responseText) as { id: string; name: string; size: number };
        currentDraft = { id: body.id, name: body.name, size: body.size };
        renderDraftChip(currentDraft);
        syncSendButton();
      } catch {
        /* ignore malformed response */
      }
    }
  };
  xhr.onerror = hideProgress;
  xhr.onabort = hideProgress;
  cancelBtn.onclick = () => {
    if (currentUploadXhr) currentUploadXhr.abort();
  };

  const fd = new FormData();
  fd.append('file', file);
  xhr.send(fd);
}

function renderDraftChip(draft: { id: string; name: string; size: number }): void {
  // The chip sits between the chat messages list and the input row so it
  // visually reads as "attached to the message you're about to send."
  let chip = document.getElementById('chat-draft-chip');
  if (!chip) {
    chip = document.createElement('div');
    chip.id = 'chat-draft-chip';
    chip.className = 'chat-file chat-draft-chip';
    const inputRow = document.getElementById('chat-input-row');
    inputRow?.parentElement?.insertBefore(chip, inputRow);
  }
  chip.innerHTML =
    `<span class="chat-file-icon"><svg viewBox="0 0 24 24"><path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48"/></svg></span>` +
    `<div class="chat-file-info"><div class="chat-file-name" title="${esc(draft.name)}">${esc(draft.name)}</div><div class="chat-file-size">${fmtBytes(draft.size)}</div></div>` +
    `<button class="chat-draft-remove" title="Remove" aria-label="Remove attachment"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg></button>`;
  chip.querySelector('.chat-draft-remove')?.addEventListener('click', () => {
    void clearDraft({ deleteRemote: true });
  });
}

async function clearDraft(opts: { deleteRemote: boolean }): Promise<void> {
  const draft = currentDraft;
  currentDraft = null;
  document.getElementById('chat-draft-chip')?.remove();
  syncSendButton();
  if (opts.deleteRemote && draft) {
    try {
      await fetch(
        `/api/public/rooms/${encodeURIComponent(slug)}/files/${encodeURIComponent(draft.id)}` +
          `?participantId=${encodeURIComponent(getParticipantId())}&token=${encodeURIComponent(getToken())}`,
        { method: 'DELETE' },
      );
    } catch {
      /* best-effort cleanup */
    }
  }
}

// The send button + chat-send only need to be lit when the chat is enabled
// AND there's something to send (text or a draft).
function syncSendButton(): void {
  const input = document.getElementById('chat-input') as HTMLTextAreaElement | null;
  const sendBtn = document.getElementById('chat-send') as HTMLButtonElement | null;
  if (!input || !sendBtn) return;
  // If the input itself is disabled (chat not connected), leave the send
  // button disabled too.
  if (input.disabled) {
    sendBtn.disabled = true;
    return;
  }
  const hasText = input.value.trim().length > 0;
  sendBtn.disabled = !(hasText || currentDraft !== null);
}

function sendChat(): void {
  const input = document.getElementById('chat-input') as HTMLTextAreaElement;
  const text = input.value.trim();
  const draft = currentDraft;
  if (!text && !draft) return;
  if (!sendFn) return;
  if (text) sendFn({ type: 'chat:message', text });
  if (draft) {
    sendFn({ type: 'file:share', fileId: draft.id });
    currentDraft = null;
    document.getElementById('chat-draft-chip')?.remove();
  }
  input.value = '';
  autoGrow(input);
  syncSendButton();
}

// Grow the composer with its content up to the CSS max-height, then scroll.
// The textarea is border-box (global reset), but scrollHeight excludes the
// border — so we add it back, otherwise the box shrinks ~1px on first input.
function autoGrow(el: HTMLTextAreaElement): void {
  const cs = getComputedStyle(el);
  const border = parseFloat(cs.borderTopWidth) + parseFloat(cs.borderBottomWidth);
  el.style.height = 'auto';
  el.style.height = `${el.scrollHeight + border}px`;
}

export function initChat(): void {
  document.getElementById('chat-send')?.addEventListener('click', sendChat);
  const input = document.getElementById('chat-input') as HTMLTextAreaElement | null;
  input?.addEventListener('keydown', (e) => {
    const ev = e as KeyboardEvent;
    // Enter sends; Shift+Enter falls through so the textarea inserts a newline.
    if (ev.key === 'Enter' && !ev.shiftKey) {
      ev.preventDefault();
      sendChat();
    }
  });
  // Keep the send button's enabled state in sync with input + draft state,
  // and grow the composer with multi-line content.
  input?.addEventListener('input', () => {
    if (input) autoGrow(input);
    syncSendButton();
  });
  document.getElementById('chat-attach')?.addEventListener('click', () => {
    (document.getElementById('file-input') as HTMLInputElement).click();
  });
  document.getElementById('file-input')?.addEventListener('change', (e) => {
    const target = e.target as HTMLInputElement;
    const file = target.files?.[0];
    if (file) uploadFile(file);
    target.value = '';
  });

  // Presenter-only Show / Delete buttons inside chat messages and the
  // files list. Delegated at #right-panel so dynamically-rendered rows
  // are covered without re-wiring per row.
  document.getElementById('right-panel')?.addEventListener('click', (e) => {
    const btn = (e.target as HTMLElement).closest<HTMLElement>('[data-action]');
    if (!btn) return;
    if (viewerStore.get().role !== 'presenter') return;
    const action = btn.dataset['action'];
    const fileId = btn.dataset['fileId'] || '';
    if (!fileId) return;

    if (action === 'display-show') {
      if (!sendFn) return;
      e.preventDefault();
      const current = viewerStore.get().displayFile?.fileId === fileId;
      sendFn({ type: 'display:set', fileId: current ? null : fileId });
    } else if (action === 'file-delete') {
      e.preventDefault();
      void hostDeleteFile(fileId);
    }
  });

  syncSendButton();
}

async function hostDeleteFile(fileId: string): Promise<void> {
  try {
    const res = await fetch(
      `/api/public/rooms/${encodeURIComponent(slug)}/files/${encodeURIComponent(fileId)}` +
        `?participantId=${encodeURIComponent(getParticipantId())}&token=${encodeURIComponent(getToken())}`,
      { method: 'DELETE' },
    );
    if (!res.ok && res.status !== 204) {
      // Backend rejected (e.g. lost host role mid-session). The optimistic
      // remove is intentionally skipped — we wait for the file:removed
      // broadcast which only fires on a successful server-side delete.
      console.warn('[host delete] failed', res.status);
    }
  } catch (err) {
    console.error('[host delete]', err);
  }
}

// Remove a file's chat message + files-list row in response to a
// file:removed broadcast. If it was the currently-displayed file the
// player will clear it independently via display:state.
export function removeFileEverywhere(fileId: string): void {
  const sel = `[data-file-id="${CSS.escape(fileId)}"]`;
  // Remove every chat message that hosts this file.
  for (const fileEl of Array.from(document.querySelectorAll<HTMLElement>(sel))) {
    const msg = fileEl.closest('.chat-msg');
    (msg ?? fileEl).remove();
  }
  // Remove the files-list row.
  const row = document
    .getElementById('files-list')
    ?.querySelector<HTMLElement>(`[data-fid="${CSS.escape(fileId)}"]`);
  row?.remove();
  // Update the files-count badge.
  const list = document.getElementById('files-list');
  const count = document.getElementById('files-count');
  if (list && count) count.textContent = String(list.querySelectorAll('.file-row').length);
  // If the list is now empty, restore the placeholder.
  if (list && !list.querySelector('.file-row') && !list.querySelector('#files-empty')) {
    const empty = document.createElement('div');
    empty.id = 'files-empty';
    empty.textContent = 'No files shared yet.';
    list.appendChild(empty);
  }
}
