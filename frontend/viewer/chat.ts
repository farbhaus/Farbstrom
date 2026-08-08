// Chat sidebar + file sharing (upload XHR with cancel + progress).

import { esc, fmtBytes, toast } from '../shared/utils.js';
import { setChatOpen, switchPanelTab } from './layout.js';
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
  const showBtn = isPresenter && canShow(msg.mime) ? showBtnHtml(msg.id, 'shared-file-show') : '';
  const delBtn = isPresenter ? deleteBtnHtml(msg.id, 'shared-file-del') : '';
  const d = document.createElement('div');
  d.className = 'chat-msg';
  // The file element reuses the Files-tab `.shared-file` look so a shared file
  // reads identically in both tabs. (The draft chip keeps `.chat-file`.)
  d.innerHTML =
    `<div class="chat-meta"><span class="chat-who ${esc(msg.role)}">${esc(msg.uploaderName)}</span><span class="chat-time">${fmtTime(msg.ts)}</span></div>` +
    `<div class="shared-file" data-file-id="${esc(msg.id)}" data-mime="${esc(msg.mime || '')}">` +
    `<div class="shared-file-name" title="${esc(msg.name)}">${esc(msg.name)}</div>` +
    `<span class="shared-file-size">${fmtBytes(msg.size)}</span>` +
    showBtn +
    `<a class="shared-file-dl" href="${url}" download="${esc(msg.name)}">Get</a>` +
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
  const showBtn = isPresenter && canShow(f.mime) ? showBtnHtml(f.id, 'shared-file-show') : '';
  const delBtn = isPresenter ? deleteBtnHtml(f.id, 'shared-file-del') : '';
  const row = document.createElement('div');
  row.className = 'shared-file';
  row.dataset['fid'] = f.id;
  if (f.mime) row.dataset['mime'] = f.mime;
  row.innerHTML =
    `<div class="shared-file-name" title="${esc(f.name)}">${esc(f.name)}</div>` +
    `<span class="shared-file-size">${fmtBytes(f.size)}</span>` +
    showBtn +
    `<a class="shared-file-dl" href="${url}" download="${esc(f.name)}">Get</a>` +
    delBtn;
  list.appendChild(row);
  const count = document.getElementById('files-count');
  if (count) count.textContent = String(list.querySelectorAll('.shared-file').length);
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

// ---- Attachment staging ----
//
// Attachments upload immediately with `defer=true` so the transfer overlaps
// with the user still typing, but they stay invisible to the room until Send
// promotes each one with a `file:share`. Uploads run strictly one at a time:
// an attachment here can be up to the backend's 2.5 GB cap, and parallel XHRs
// would only split the same uplink.

const MAX_DRAFTS = 10;
const CLIP_SVG =
  '<svg viewBox="0 0 24 24"><path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48"/></svg>';

type DraftState = 'queued' | 'uploading' | 'ready' | 'failed';

interface Draft {
  /** DOM key — deliberately not the server id, which doesn't exist yet. */
  localId: string;
  file: File;
  /** Server-side id, set once the deferred upload lands. */
  fileId: string | null;
  name: string;
  size: number;
  state: DraftState;
  pct: number;
  xhr: XMLHttpRequest | null;
}

let drafts: Draft[] = [];
let draftSeq = 0;

/** Chat has to be connected before anything can be staged onto it. */
function chatAcceptsFiles(): boolean {
  const input = document.getElementById('chat-input') as HTMLTextAreaElement | null;
  return !!input && !input.disabled;
}

// Single entry point for both the paperclip picker and a panel drop.
function enqueueFiles(files: File[]): void {
  if (!files.length || !chatAcceptsFiles()) return;
  const room = MAX_DRAFTS - drafts.length;
  if (room <= 0) {
    toast(`Up to ${MAX_DRAFTS} attachments at a time`);
    return;
  }
  const accepted = files.slice(0, room);
  if (accepted.length < files.length) {
    toast(`Up to ${MAX_DRAFTS} attachments at a time — ${files.length - accepted.length} skipped`);
  }
  for (const file of accepted) {
    drafts.push({
      localId: `d${++draftSeq}`,
      file,
      fileId: null,
      name: file.name,
      size: file.size,
      state: 'queued',
      pct: 0,
      xhr: null,
    });
  }
  renderDrafts();
  syncSendButton();
  pumpQueue();
}

function pumpQueue(): void {
  if (drafts.some((d) => d.state === 'uploading')) return;
  const next = drafts.find((d) => d.state === 'queued');
  if (next) startUpload(next);
}

function startUpload(draft: Draft): void {
  draft.state = 'uploading';
  draft.pct = 0;
  renderDrafts();

  const xhr = new XMLHttpRequest();
  draft.xhr = xhr;
  xhr.open(
    'POST',
    `/api/public/rooms/${encodeURIComponent(slug)}/files` +
      `?participantId=${encodeURIComponent(getParticipantId())}` +
      `&token=${encodeURIComponent(getToken())}&defer=true`,
  );
  xhr.upload.addEventListener('progress', (e) => {
    if (!e.lengthComputable) return;
    draft.pct = Math.round((e.loaded / e.total) * 100);
    // Patch the chip in place — a full re-render on every tick would recreate
    // the fill element and kill its width transition.
    updateDraftProgress(draft);
  });
  const finish = (state: DraftState): void => {
    draft.xhr = null;
    draft.state = state;
    renderDrafts();
    syncSendButton();
    pumpQueue();
  };
  xhr.onload = () => {
    if (xhr.status >= 200 && xhr.status < 300) {
      try {
        const body = JSON.parse(xhr.responseText) as { id: string; name: string; size: number };
        draft.fileId = body.id;
        draft.name = body.name;
        draft.size = body.size;
        finish('ready');
        return;
      } catch {
        /* malformed response — fall through to the failure path */
      }
    }
    finish('failed');
  };
  xhr.onerror = () => finish('failed');
  // No onabort: removeDraft() has already dropped the draft and re-pumped the
  // queue by the time abort() fires.

  const fd = new FormData();
  fd.append('file', draft.file);
  xhr.send(fd);
}

function draftSubtitle(draft: Draft): string {
  switch (draft.state) {
    case 'queued':
      return `${fmtBytes(draft.size)} · waiting`;
    case 'uploading':
      return `${fmtBytes(draft.size)} · ${draft.pct}%`;
    case 'failed':
      return 'Upload failed';
    default:
      return fmtBytes(draft.size);
  }
}

function draftChipHtml(draft: Draft): string {
  const inFlight = draft.state === 'queued' || draft.state === 'uploading';
  const label = inFlight ? 'Cancel upload' : 'Remove attachment';
  return (
    `<div class="chat-file chat-draft-chip${draft.state === 'failed' ? ' is-failed' : ''}" data-draft-id="${draft.localId}">` +
    `<span class="chat-file-icon">${CLIP_SVG}</span>` +
    `<div class="chat-file-info">` +
    `<div class="chat-file-name" title="${esc(draft.name)}">${esc(draft.name)}</div>` +
    `<div class="chat-file-size">${esc(draftSubtitle(draft))}</div>` +
    `</div>` +
    `<button class="chat-draft-remove" data-draft-id="${draft.localId}" title="${label}" aria-label="${label}">` +
    `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>` +
    `</button>` +
    (inFlight
      ? `<div class="chat-draft-progress"><div style="width:${draft.pct}%"></div></div>`
      : '') +
    `</div>`
  );
}

// The tray sits between the messages list and the input row so the chips read
// as "attached to the message you're about to send."
function renderDrafts(): void {
  const list = document.getElementById('chat-draft-list');
  if (!list) return;
  list.innerHTML = drafts.map(draftChipHtml).join('');
}

function chipEl(localId: string): HTMLElement | null {
  return document.querySelector<HTMLElement>(
    `#chat-draft-list [data-draft-id="${CSS.escape(localId)}"]`,
  );
}

function updateDraftProgress(draft: Draft): void {
  const chip = chipEl(draft.localId);
  if (!chip) return;
  const fill = chip.querySelector<HTMLElement>('.chat-draft-progress > div');
  if (fill) fill.style.width = draft.pct + '%';
  const sub = chip.querySelector<HTMLElement>('.chat-file-size');
  if (sub) sub.textContent = draftSubtitle(draft);
}

function removeDraft(localId: string): void {
  const idx = drafts.findIndex((d) => d.localId === localId);
  if (idx === -1) return;
  const draft = drafts.splice(idx, 1)[0];
  if (!draft) return;
  draft.xhr?.abort();
  draft.xhr = null;
  renderDrafts();
  syncSendButton();
  pumpQueue();
  // Only a completed upload left something on the server to clean up.
  if (draft.fileId) void deleteRemoteDraft(draft.fileId);
}

async function deleteRemoteDraft(fileId: string): Promise<void> {
  try {
    await fetch(
      `/api/public/rooms/${encodeURIComponent(slug)}/files/${encodeURIComponent(fileId)}` +
        `?participantId=${encodeURIComponent(getParticipantId())}&token=${encodeURIComponent(getToken())}`,
      { method: 'DELETE' },
    );
  } catch {
    /* best-effort cleanup */
  }
}

// The send button only needs to be lit when the chat is enabled AND there's
// something to send (text, or an attachment that finished uploading).
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
  sendBtn.disabled = !(hasText || drafts.some((d) => d.state === 'ready'));
}

function sendChat(): void {
  const input = document.getElementById('chat-input') as HTMLTextAreaElement;
  const text = input.value.trim();
  const ready = drafts.filter((d) => d.state === 'ready');
  if (!text && !ready.length) return;
  if (!sendFn) return;
  if (text) sendFn({ type: 'chat:message', text });
  for (const draft of ready) {
    if (draft.fileId) sendFn({ type: 'file:share', fileId: draft.fileId });
  }
  // Attachments still uploading stay staged, so a large transfer never blocks
  // sending a message — the user hits Send again once it lands.
  if (ready.length) {
    drafts = drafts.filter((d) => d.state !== 'ready');
    renderDrafts();
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

// ---- Drag and drop (#211) ----
//
// The whole viewer is the drop target — a file dropped over the video, the
// roster, anywhere, is staged in chat. The panel is opened on drop so the
// staged chip is visible rather than hidden behind a closed panel.

/** True only for an OS file drag — not a text selection or an in-page drag. */
function isFileDrag(e: DragEvent): boolean {
  const types = e.dataTransfer?.types;
  return !!types && Array.from(types).includes('Files');
}

// Pull real files out of a drop. `items` is preferred over `files` so a dropped
// *folder* — easy to grab by accident when the shot lives in one — is reported
// rather than uploaded as a 0-byte entry.
function collectFiles(dt: DataTransfer | null): File[] {
  if (!dt) return [];
  const items = Array.from(dt.items ?? []);
  if (!items.length) return Array.from(dt.files);
  const files: File[] = [];
  let folders = 0;
  for (const item of items) {
    if (item.kind !== 'file') continue;
    // webkitGetAsEntry is the only synchronous way to tell a directory from a
    // file; it's non-standard but universal, and absent entries fall through
    // to getAsFile so nothing is lost if it ever goes away.
    const entry = item.webkitGetAsEntry();
    if (entry && !entry.isFile) {
      folders++;
      continue;
    }
    const file = item.getAsFile();
    if (file) files.push(file);
  }
  if (folders) toast('Folders can’t be attached');
  return files;
}

function initDropZone(): void {
  const overlay = document.getElementById('chat-drop-overlay');
  if (!overlay) return;

  const disarm = (): void => overlay.classList.remove('is-active');

  // Every file drag is preventDefault'd whether or not we can accept it: the
  // browser's default is to navigate the tab to the dropped file, which
  // silently ends the session. When chat isn't connected we take the drag but
  // show 'none', so the cursor stays honest instead of promising a drop.
  const arm = (e: DragEvent): void => {
    if (!isFileDrag(e)) return;
    e.preventDefault();
    const ok = chatAcceptsFiles();
    if (e.dataTransfer) e.dataTransfer.dropEffect = ok ? 'copy' : 'none';
    overlay.classList.toggle('is-active', ok);
  };
  window.addEventListener('dragenter', arm);
  window.addEventListener('dragover', arm);

  // Once armed the overlay covers the viewport and is the topmost hit-test
  // target, so its own dragleave fires exactly once — when the pointer really
  // leaves the window. No per-element enter/leave bookkeeping needed.
  overlay.addEventListener('dragleave', disarm);
  // Belt and braces: a drag cancelled in-window (Escape) doesn't always send a
  // dragleave, and a stuck overlay would swallow the whole page.
  window.addEventListener('dragend', disarm);

  window.addEventListener('drop', (e) => {
    if (!isFileDrag(e)) return;
    e.preventDefault();
    disarm();
    if (!chatAcceptsFiles()) return;
    const files = collectFiles(e.dataTransfer);
    if (!files.length) return;
    // Fullscreen puts #stage in the top layer, above the panel — leave it, or
    // the staged chip lands somewhere the user can't see and the drop reads as
    // having done nothing.
    if (document.fullscreenElement || document.webkitFullscreenElement) {
      void (document.exitFullscreen || document.webkitExitFullscreen)?.call(document);
    }
    // Drop lands anywhere on the page, so surface where the files went:
    // reveal the panel and its Chat tab before staging.
    setChatOpen(true);
    switchPanelTab('chat');
    enqueueFiles(files);
  });
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
    enqueueFiles(Array.from(target.files ?? []));
    target.value = '';
  });
  // Remove / cancel on a staged attachment. Delegated so the tray can be
  // re-rendered freely without re-wiring per chip.
  document.getElementById('chat-draft-list')?.addEventListener('click', (e) => {
    const btn = (e.target as HTMLElement).closest<HTMLElement>('.chat-draft-remove');
    const id = btn?.dataset['draftId'];
    if (id) removeDraft(id);
  });

  initDropZone();

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
  if (list && count) count.textContent = String(list.querySelectorAll('.shared-file').length);
  // If the list is now empty, restore the placeholder.
  if (list && !list.querySelector('.shared-file') && !list.querySelector('#files-empty')) {
    const empty = document.createElement('div');
    empty.id = 'files-empty';
    empty.textContent = 'No files shared yet.';
    list.appendChild(empty);
  }
}
