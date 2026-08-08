import { apiFetch, getToken } from './auth.js';
import { closeModal, openModal } from '../shared/components.js';
import { esc, fmtBytes, fmtDate, toast } from '../shared/utils.js';
import type { FileEntry, Room, StorageStats } from './types.js';

interface MimeCategory {
  key: string;
  label: string;
  color: string;
  match: (mime: string) => boolean;
}

// Colors are CSS custom properties, not hex literals: they land in an inline
// `style="background:…"` (see renderStorage below), so var() resolves normally
// and the palette stays defined once in www/shared/tokens.css. This also means
// the PDF/Docs/Other segments track --danger/--green/--dim under a rebrand,
// which the previous near-miss hexes (#ff5252, #4caf50, #777) did not.
const MIME_CATEGORIES: MimeCategory[] = [
  { key: 'image', label: 'Images', color: 'var(--cat-image)', match: (m) => m.startsWith('image/') },
  { key: 'video', label: 'Video', color: 'var(--cat-video)', match: (m) => m.startsWith('video/') },
  { key: 'audio', label: 'Audio', color: 'var(--cat-audio)', match: (m) => m.startsWith('audio/') },
  { key: 'pdf', label: 'PDF', color: 'var(--cat-pdf)', match: (m) => m === 'application/pdf' },
  {
    key: 'doc',
    label: 'Docs',
    color: 'var(--cat-doc)',
    match: (m) => m.includes('openxmlformats') || m === 'text/plain',
  },
  { key: 'other', label: 'Other', color: 'var(--cat-other)', match: () => true },
];

function mimeCategory(mime: string): MimeCategory {
  for (const c of MIME_CATEGORIES) if (c.match(mime)) return c;
  return MIME_CATEGORIES[MIME_CATEGORIES.length - 1]!;
}

function fileIconEmoji(mime: string): string {
  if (mime.startsWith('image/')) return '🖼';
  if (mime.startsWith('video/')) return '🎬';
  if (mime.startsWith('audio/')) return '🎵';
  if (mime === 'application/pdf') return '📕';
  if (mime.includes('openxmlformats-officedocument.wordprocessingml')) return '📄';
  if (mime.includes('openxmlformats-officedocument.spreadsheetml')) return '📊';
  if (mime.includes('zip')) return '🗜';
  return '📁';
}

let filesList: FileEntry[] = [];
const filesSelected = new Set<string>();
let filesSearchTimer: ReturnType<typeof setTimeout> | null = null;
let filesReplaceId: string | null = null;
let filesAssignTargetIds: string[] = [];
let filesDeleteTargetIds: string[] = [];

let getRooms: () => Room[] = () => [];

export function configureFiles(opts: { getRooms: () => Room[] }): void {
  getRooms = opts.getRooms;
}

function input(id: string): HTMLInputElement {
  return document.getElementById(id) as HTMLInputElement;
}

function filesQueryString(): string {
  const params = new URLSearchParams();
  const s = input('files-search').value.trim();
  const m = (document.getElementById('files-type-filter') as HTMLSelectElement).value;
  const sortVal = (document.getElementById('files-sort') as HTMLSelectElement).value;
  const [sort, order] = sortVal.split('|');
  const unassigned = input('files-unassigned-only').checked;
  if (s) params.set('search', s);
  if (m) params.set('mimePrefix', m);
  if (sort) params.set('sort', sort);
  if (order) params.set('order', order);
  if (unassigned) params.set('unassigned', '1');
  const qs = params.toString();
  return qs ? '?' + qs : '';
}

export async function loadFiles(): Promise<void> {
  const res = await apiFetch('/api/admin/files' + filesQueryString());
  if (!res || !res.ok) return;
  filesList = await res.json();
  pruneSelection();
  renderFiles();
}

// Drop anything that fell out of the listing (a filter, a search, a delete)
// from the selection. Without this the bulk bar keeps counting rows that
// aren't on screen, and "Delete selected" reaches files the admin can't see.
function pruneSelection(): void {
  if (!filesSelected.size) return;
  const listed = new Set(filesList.map((f) => f.id));
  for (const id of Array.from(filesSelected)) {
    if (!listed.has(id)) filesSelected.delete(id);
  }
}

export async function loadStorageStats(): Promise<void> {
  const res = await apiFetch('/api/admin/files/stats');
  if (!res || !res.ok) return;
  const stats: StorageStats = await res.json();
  renderStorageWidget(stats);
}

interface Bucket {
  count: number;
  bytes: number;
  label: string;
  color: string;
}

function renderStorageWidget(stats: StorageStats): void {
  const total = stats.totalBytes || 0;
  const count = stats.totalCount || 0;
  const totalEl = document.getElementById('files-storage-total');
  const countEl = document.getElementById('files-storage-count');
  if (totalEl) totalEl.textContent = fmtBytes(total);
  if (countEl) countEl.textContent = `${count} file${count === 1 ? '' : 's'}`;

  const buckets: Record<string, Bucket> = {};
  for (const c of MIME_CATEGORIES) {
    buckets[c.key] = { count: 0, bytes: 0, label: c.label, color: c.color };
  }
  for (const b of stats.byMime || []) {
    const cat = mimeCategory(b.prefix || '');
    const bucket = buckets[cat.key]!;
    bucket.count += b.count || 0;
    bucket.bytes += b.bytes || 0;
  }
  const bar = document.getElementById('files-storage-bar');
  const legend = document.getElementById('files-storage-legend');
  if (!bar || !legend) return;
  if (total === 0) {
    bar.innerHTML = '<span style="width:100%;background:var(--surface2)"></span>';
    legend.innerHTML =
      '<span class="files-storage-legend-item" style="color:var(--faint)">No files yet</span>';
    return;
  }
  const values = Object.values(buckets);
  bar.innerHTML = values
    .filter((b) => b.bytes > 0)
    .map(
      (b) =>
        `<span style="width:${((b.bytes / total) * 100).toFixed(2)}%;background:${b.color}" title="${esc(b.label)}: ${fmtBytes(b.bytes)}"></span>`,
    )
    .join('');
  legend.innerHTML = values
    .filter((b) => b.count > 0)
    .map(
      (b) =>
        `<span class="files-storage-legend-item"><span class="files-storage-legend-dot" style="background:${b.color}"></span>${esc(b.label)} · ${b.count} · ${fmtBytes(b.bytes)}</span>`,
    )
    .join('');
}

function renderFiles(): void {
  const container = document.getElementById('files-list');
  if (!container) return;
  if (!filesList.length) {
    container.innerHTML = '<div class="empty">No files. Drop files here or click Upload.</div>';
    updateBulkBar();
    return;
  }
  const token = getToken();
  container.innerHTML = filesList
    .map((f) => {
      const checked = filesSelected.has(f.id) ? 'checked' : '';
      const selClass = filesSelected.has(f.id) ? ' selected' : '';
      const icon =
        f.mime && f.mime.startsWith('image/')
          ? `<div class="file-icon" data-action="preview" data-id="${esc(f.id)}"><img src="/api/admin/files/${esc(f.id)}/preview?token=${encodeURIComponent(token)}" alt=""></div>`
          : `<div class="file-icon" data-action="preview" data-id="${esc(f.id)}">${fileIconEmoji(f.mime || '')}</div>`;
      const chips =
        f.assignedRooms && f.assignedRooms.length
          ? f.assignedRooms
              .map(
                (r) =>
                  `<span class="file-chip">${esc(r.name || r.slug)}<button data-action="unassign" data-room="${esc(r.id)}" data-id="${esc(f.id)}" title="Unassign">×</button></span>`,
              )
              .join('')
          : '<span class="file-chip-empty">Unassigned</span>';
      const dateStr = f.createdAt ? fmtDate(f.createdAt) : '';
      return `
      <div class="file-row${selClass}" data-id="${esc(f.id)}">
        <input type="checkbox" data-action="select" data-id="${esc(f.id)}" ${checked}>
        ${icon}
        <div class="file-name" data-action="rename" data-id="${esc(f.id)}" title="${esc(f.name)}">${esc(f.name)}</div>
        <div class="file-meta file-meta-type">${esc((f.mime || '').split('/').pop() || '—')}</div>
        <div class="file-meta file-meta-size">${fmtBytes(f.size || 0)}</div>
        <div class="file-meta file-meta-date">${esc(dateStr || '')}</div>
        <div class="file-chips">${chips}</div>
        <div class="file-actions">
          <button class="btn btn-sm" data-action="assign-one" data-id="${esc(f.id)}" title="Assign">Assign</button>
          <button class="btn btn-sm" data-action="replace" data-id="${esc(f.id)}" title="Replace">Replace</button>
          <button class="btn btn-sm" data-action="download" data-id="${esc(f.id)}" title="Download">↓</button>
          <button class="btn btn-sm btn-danger" data-action="delete-one" data-id="${esc(f.id)}" title="Delete">✕</button>
        </div>
      </div>`;
    })
    .join('');
  updateBulkBar();
}

function updateBulkBar(): void {
  const bar = document.getElementById('files-bulk-bar');
  const count = document.getElementById('files-bulk-count');
  if (!bar) return;
  if (filesSelected.size > 0) {
    bar.classList.add('active');
    if (count) count.textContent = String(filesSelected.size);
  } else {
    bar.classList.remove('active');
  }
  syncSelectAll();
}

// ---- Selection ----

// The header checkbox reflects the listed files. pruneSelection keeps the two
// in step, so this is really "is everything on screen ticked".
function syncSelectAll(): void {
  const box = document.getElementById('files-select-all') as HTMLInputElement | null;
  if (!box) return;
  const listed = filesList.length;
  const picked = filesList.filter((f) => filesSelected.has(f.id)).length;
  box.checked = listed > 0 && picked === listed;
  box.indeterminate = picked > 0 && picked < listed;
  box.disabled = listed === 0;
}

// Push `filesSelected` onto the rendered rows without a full re-render — the
// checkbox drag calls this on every pointermove.
function paintSelection(): void {
  const container = document.getElementById('files-list');
  if (!container) return;
  for (const row of Array.from(container.querySelectorAll<HTMLElement>('.file-row'))) {
    const id = row.dataset['id'] || '';
    const on = filesSelected.has(id);
    row.classList.toggle('selected', on);
    const cb = row.querySelector<HTMLInputElement>('input[data-action="select"]');
    if (cb) cb.checked = on;
  }
  updateBulkBar();
}

function selectAllListed(on: boolean): void {
  for (const f of filesList) {
    if (on) filesSelected.add(f.id);
    else filesSelected.delete(f.id);
  }
  paintSelection();
}

// ---- Drag across the checkboxes ----
//
// Press a row's checkbox and drag up or down: every row the pointer passes
// takes the *opposite* of the state you started on. So a drag begun on a
// ticked box clears the run, and one begun on an empty box fills it.
//
// A plain click is left entirely alone — the native checkbox toggle and the
// existing 'select' action handle it. Only once the pointer actually moves do
// we take over, and then we suppress the trailing click so the browser doesn't
// toggle the anchor row a second time on top of what we already applied.

const DRAG_THRESHOLD = 3; // px of travel before a click counts as a drag

function initCheckboxDrag(zone: HTMLElement): void {
  let rows: HTMLElement[] = [];
  let bands: Array<{ top: number; bottom: number }> = [];
  let anchor = -1;
  let want = false;
  let startY = 0;
  let dragging = false;
  let pointerId = -1;

  // Page coordinates, not viewport: the run has to stay glued to the rows if
  // the page scrolls mid-drag.
  const rowAt = (pageY: number): number => {
    for (let i = 0; i < bands.length; i++) {
      const b = bands[i];
      if (b && pageY >= b.top && pageY <= b.bottom) return i;
    }
    // Past either end — clamp, so overshooting keeps extending the run.
    const first = bands[0];
    if (first && pageY < first.top) return 0;
    return bands.length - 1;
  };

  const applyRun = (to: number): void => {
    const lo = Math.min(anchor, to);
    const hi = Math.max(anchor, to);
    for (let i = lo; i <= hi; i++) {
      const id = rows[i]?.dataset['id'];
      if (!id) continue;
      if (want) filesSelected.add(id);
      else filesSelected.delete(id);
    }
    paintSelection();
  };

  zone.addEventListener('pointerdown', (e) => {
    if (e.button !== 0 || e.pointerType === 'touch') return;
    const cb = (e.target as HTMLElement).closest<HTMLInputElement>('input[data-action="select"]');
    if (!cb) return;
    const list = document.getElementById('files-list');
    rows = list ? Array.from(list.querySelectorAll<HTMLElement>('.file-row')) : [];
    anchor = rows.findIndex((r) => r.contains(cb));
    if (anchor === -1) return;

    // Snapshot geometry once — the list can't change mid-drag.
    bands = rows.map((r) => {
      const rect = r.getBoundingClientRect();
      return { top: rect.top + window.scrollY, bottom: rect.bottom + window.scrollY };
    });
    want = !cb.checked;
    startY = e.pageY;
    dragging = false;
    pointerId = e.pointerId;
    // Deliberately NOT capturing the pointer here. Capture retargets the
    // trailing click to the capture element, which robs the checkbox of its
    // native toggle and stops the delegated 'select' action from matching —
    // i.e. a plain click would do nothing. Capture is taken in pointermove,
    // once this is genuinely a drag and we're going to swallow the click anyway.
  });

  zone.addEventListener('pointermove', (e) => {
    if (anchor === -1 || e.pointerId !== pointerId) return;
    if (!dragging) {
      if (Math.abs(e.pageY - startY) < DRAG_THRESHOLD) return;
      dragging = true;
      zone.classList.add('checkbox-drag');
      // Now that it's a drag, capture so moves that stray off the list still
      // extend the run.
      zone.setPointerCapture(e.pointerId);
    }
    applyRun(rowAt(e.pageY));
  });

  // On window, not the zone: without capture a release outside the list would
  // never reach a zone-scoped listener and the drag state would stick.
  const end = (e: PointerEvent): void => {
    if (anchor === -1 || e.pointerId !== pointerId) return;
    if (zone.hasPointerCapture(e.pointerId)) zone.releasePointerCapture(e.pointerId);
    zone.classList.remove('checkbox-drag');
    if (dragging) {
      // Swallow the trailing click: we've already set the anchor row, and the
      // native checkbox activation would flip it straight back. Dropped on the
      // next tick in case no click materialises — an armed once-listener would
      // otherwise eat an unrelated click later on.
      const swallow = (ev: MouseEvent): void => {
        ev.stopPropagation();
        ev.preventDefault();
      };
      window.addEventListener('click', swallow, { capture: true, once: true });
      setTimeout(() => window.removeEventListener('click', swallow, { capture: true }), 0);
    }
    dragging = false;
    anchor = -1;
    pointerId = -1;
  };
  window.addEventListener('pointerup', end);
  window.addEventListener('pointercancel', end);
}

// ---- Upload queue ----

let uploadQueueSeq = 0;
const uploadsInFlight = new Map<string, XMLHttpRequest>();

function ensureUploadQueueVisible(): void {
  const q = document.getElementById('files-upload-queue');
  if (q) q.classList.toggle('active', q.children.length > 0);
}

function addUploadRow(file: File): string {
  const id = 'up-' + ++uploadQueueSeq;
  const row = document.createElement('div');
  row.className = 'upload-item';
  row.id = id;
  row.innerHTML =
    `<div class="upload-item-name" title="${esc(file.name)}">${esc(file.name)}</div>` +
    `<div class="upload-item-bar"><span></span></div>` +
    `<div class="upload-item-pct">0%</div>` +
    `<button class="upload-item-cancel" title="Cancel">✕</button>`;
  document.getElementById('files-upload-queue')?.appendChild(row);
  ensureUploadQueueVisible();
  return id;
}

function setUploadProgress(rowId: string, pct: number): void {
  const row = document.getElementById(rowId);
  if (!row) return;
  const span = row.querySelector('.upload-item-bar > span') as HTMLElement | null;
  const pctEl = row.querySelector('.upload-item-pct') as HTMLElement | null;
  if (span) span.style.width = pct + '%';
  if (pctEl) pctEl.textContent = pct + '%';
}

function finishUploadRow(rowId: string, outcome: 'done' | 'error' | 'cancelled'): void {
  const row = document.getElementById(rowId);
  if (!row) return;
  const bar = row.querySelector('.upload-item-bar');
  const pctEl = row.querySelector('.upload-item-pct');
  bar?.classList.remove('done', 'error');
  if (outcome === 'done') {
    bar?.classList.add('done');
    if (pctEl) pctEl.textContent = 'Done';
  } else if (outcome === 'error') {
    bar?.classList.add('error');
    if (pctEl) pctEl.textContent = 'Failed';
  } else {
    bar?.classList.add('error');
    if (pctEl) pctEl.textContent = 'Cancelled';
  }
  const btn = row.querySelector('.upload-item-cancel') as HTMLButtonElement | null;
  if (btn) btn.disabled = true;
  setTimeout(() => {
    row.remove();
    ensureUploadQueueVisible();
  }, 2500);
}

function uploadOne(file: File): Promise<boolean> {
  const rowId = addUploadRow(file);
  const xhr = new XMLHttpRequest();
  uploadsInFlight.set(rowId, xhr);

  const row = document.getElementById(rowId);
  row?.querySelector('.upload-item-cancel')?.addEventListener('click', () => xhr.abort());

  xhr.open('POST', '/api/admin/files');
  xhr.setRequestHeader('Authorization', `Bearer ${getToken()}`);
  xhr.upload.addEventListener('progress', (e) => {
    if (e.lengthComputable) {
      setUploadProgress(rowId, Math.round((e.loaded / e.total) * 100));
    }
  });

  return new Promise((resolve) => {
    xhr.onload = () => {
      uploadsInFlight.delete(rowId);
      if (xhr.status >= 200 && xhr.status < 300) {
        setUploadProgress(rowId, 100);
        finishUploadRow(rowId, 'done');
        resolve(true);
      } else {
        finishUploadRow(rowId, 'error');
        resolve(false);
      }
    };
    xhr.onerror = () => {
      uploadsInFlight.delete(rowId);
      finishUploadRow(rowId, 'error');
      resolve(false);
    };
    xhr.onabort = () => {
      uploadsInFlight.delete(rowId);
      finishUploadRow(rowId, 'cancelled');
      resolve(false);
    };

    const fd = new FormData();
    fd.append('file', file);
    xhr.send(fd);
  });
}

async function uploadFiles(files: File[]): Promise<void> {
  if (!files.length) return;
  // Start all uploads in parallel — XHR is concurrent, users can cancel each.
  const results = await Promise.all(files.map(uploadOne));
  const ok = results.filter(Boolean).length;
  if (ok > 0) {
    void loadFiles();
    void loadStorageStats();
  }
  if (ok < files.length) toast(`${ok}/${files.length} uploaded`);
}

// ---- Inline rename ----

function startRename(cell: HTMLElement): void {
  if (cell.classList.contains('editing')) return;
  const id = cell.getAttribute('data-id') || '';
  const current = cell.textContent || '';
  cell.classList.add('editing');
  cell.innerHTML = `<input class="file-name-input" type="text" value="${esc(current)}">`;
  const inputEl = cell.querySelector('input') as HTMLInputElement;
  inputEl.focus();
  inputEl.select();
  const finish = async (commit: boolean): Promise<void> => {
    cell.classList.remove('editing');
    if (commit && inputEl.value.trim() && inputEl.value !== current) {
      const res = await apiFetch(`/api/admin/files/${id}`, {
        method: 'PATCH',
        body: JSON.stringify({ originalName: inputEl.value.trim() }),
      });
      if (res && res.ok) {
        toast('Renamed');
        void loadFiles();
        return;
      }
      toast('Rename failed');
    }
    cell.textContent = current;
  };
  inputEl.addEventListener('blur', () => void finish(true));
  inputEl.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      inputEl.blur();
    }
    if (e.key === 'Escape') {
      e.preventDefault();
      void finish(false);
    }
  });
}

// ---- Assign modal ----

function openAssignModal(fileIds: string[]): void {
  filesAssignTargetIds = fileIds;
  const title =
    fileIds.length === 1 ? 'Assign to Rooms' : `Assign ${fileIds.length} Files to Rooms`;
  const titleEl = document.getElementById('files-assign-title');
  if (titleEl) titleEl.textContent = title;

  const preChecked = new Set<string>();
  if (fileIds.length === 1) {
    const f = filesList.find((x) => x.id === fileIds[0]);
    if (f && f.assignedRooms) f.assignedRooms.forEach((r) => preChecked.add(r.id));
  }
  const list = document.getElementById('files-assign-list');
  if (!list) return;
  const rooms = getRooms();
  if (!rooms.length) {
    list.innerHTML = '<div class="empty">No rooms yet. Create one first.</div>';
  } else {
    list.innerHTML = rooms
      .map(
        (r) => `
        <label class="assign-room-row">
          <input type="checkbox" value="${esc(r.id)}" ${preChecked.has(r.id) ? 'checked' : ''}>
          <span class="assign-room-name">${esc(r.name)}</span>
          <span class="assign-room-slug">/${esc(r.slug)}</span>
        </label>`,
      )
      .join('');
  }
  openModal('files-assign-modal');
}

async function saveAssign(): Promise<void> {
  const checked = Array.from(
    document.querySelectorAll<HTMLInputElement>(
      '#files-assign-list input[type=checkbox]:checked',
    ),
  ).map((c) => c.value);
  const unchecked = Array.from(
    document.querySelectorAll<HTMLInputElement>(
      '#files-assign-list input[type=checkbox]:not(:checked)',
    ),
  ).map((c) => c.value);

  if (filesAssignTargetIds.length === 1) {
    const f = filesList.find((x) => x.id === filesAssignTargetIds[0]);
    if (!f) return;
    const current = new Set((f.assignedRooms || []).map((r) => r.id));
    const toAdd = checked.filter((id) => !current.has(id));
    const toRemove = unchecked.filter((id) => current.has(id));
    for (const rid of toAdd) {
      await apiFetch(`/api/admin/rooms/${rid}/files`, {
        method: 'POST',
        body: JSON.stringify({ fileIds: [f.id] }),
      });
    }
    for (const rid of toRemove) {
      await apiFetch(`/api/admin/rooms/${rid}/files/${f.id}`, { method: 'DELETE' });
    }
  } else {
    // Bulk: only add — don't mess with each file's existing assignments.
    for (const rid of checked) {
      await apiFetch(`/api/admin/rooms/${rid}/files`, {
        method: 'POST',
        body: JSON.stringify({ fileIds: filesAssignTargetIds }),
      });
    }
  }
  closeModal('files-assign-modal');
  filesSelected.clear();
  toast('Assignments updated');
  void loadFiles();
}

async function unassignFile(roomId: string, fileId: string): Promise<void> {
  const res = await apiFetch(`/api/admin/rooms/${roomId}/files/${fileId}`, { method: 'DELETE' });
  if (res && res.ok) {
    toast('Unassigned');
    void loadFiles();
  } else {
    toast('Unassign failed');
  }
}

// ---- Preview modal ----

function openPreviewModal(id: string): void {
  const f = filesList.find((x) => x.id === id);
  if (!f) return;
  const titleEl = document.getElementById('files-preview-title');
  if (titleEl) titleEl.textContent = f.name;
  const body = document.getElementById('files-preview-body');
  if (!body) return;
  const url = `/api/admin/files/${id}/preview?token=${encodeURIComponent(getToken())}`;
  const mime = f.mime || '';
  if (mime.startsWith('image/')) {
    body.innerHTML = `<img src="${url}" alt="">`;
  } else if (mime.startsWith('video/')) {
    body.innerHTML = `<video src="${url}" controls></video>`;
  } else if (mime.startsWith('audio/')) {
    body.innerHTML = `<audio src="${url}" controls></audio>`;
  } else if (mime === 'application/pdf') {
    body.innerHTML = `<iframe src="${url}"></iframe>`;
  } else {
    body.innerHTML =
      '<div class="preview-unsupported">Preview not available for this file type. Use Download.</div>';
  }
  const dl = document.getElementById('files-preview-download') as HTMLButtonElement | null;
  if (dl) {
    dl.onclick = () => {
      window.open(`/api/admin/files/${id}/download?token=${encodeURIComponent(getToken())}`, '_blank');
    };
  }
  openModal('files-preview-modal');
}

function closePreviewModal(): void {
  closeModal('files-preview-modal');
  const body = document.getElementById('files-preview-body');
  if (body) body.innerHTML = '';
}

// ---- Delete modal ----

function openDeleteModal(ids: string[]): void {
  filesDeleteTargetIds = ids;
  const targets = filesList.filter((f) => ids.includes(f.id));
  const lines = targets
    .map((f) => {
      const chips =
        f.assignedRooms && f.assignedRooms.length
          ? f.assignedRooms.map((r) => esc(r.name || r.slug || '')).join(', ')
          : '<span style="color:var(--faint)">no rooms</span>';
      return `<div style="margin-bottom:6px"><b>${esc(f.name)}</b><br><span style="color:var(--dim);font-size:12px">Assigned: ${chips}</span></div>`;
    })
    .join('');
  const header = ids.length === 1 ? 'Delete this file?' : `Delete ${ids.length} files?`;
  const body = document.getElementById('files-delete-body');
  if (body) {
    body.innerHTML = `<p style="margin-bottom:14px"><b>${header}</b> This cannot be undone. Viewers in assigned rooms will lose access immediately.</p>${lines}`;
  }
  openModal('files-delete-modal');
}

async function confirmDelete(): Promise<void> {
  const ids = filesDeleteTargetIds;
  if (ids.length === 1) {
    await apiFetch(`/api/admin/files/${ids[0]}`, { method: 'DELETE' });
  } else {
    await apiFetch('/api/admin/files/bulk-delete', {
      method: 'POST',
      body: JSON.stringify({ fileIds: ids }),
    });
  }
  closeModal('files-delete-modal');
  filesSelected.clear();
  toast(`${ids.length} deleted`);
  void loadFiles();
  void loadStorageStats();
}

// ---- Replace modal ----

function openReplaceModal(id: string): void {
  filesReplaceId = id;
  const f = filesList.find((x) => x.id === id);
  if (!f) return;
  const oldEl = document.getElementById('files-replace-old');
  if (oldEl) oldEl.innerHTML = `Replacing <b>${esc(f.name)}</b> (${fmtBytes(f.size || 0)})`;
  input('files-replace-input').value = '';
  const newEl = document.getElementById('files-replace-new');
  if (newEl) newEl.textContent = '';
  openModal('files-replace-modal');
}

async function confirmReplace(): Promise<void> {
  const inp = input('files-replace-input');
  const file = inp.files?.[0];
  if (!file) {
    toast('Pick a file first');
    return;
  }
  const fd = new FormData();
  fd.append('file', file);
  const res = await fetch(`/api/admin/files/${filesReplaceId}`, {
    method: 'PUT',
    headers: { Authorization: `Bearer ${getToken()}` },
    body: fd,
  });
  closeModal('files-replace-modal');
  if (res.ok) {
    toast('Replaced');
    void loadFiles();
    void loadStorageStats();
  } else {
    toast('Replace failed');
  }
}

// ---- Wire DOM ----

export function initFiles(): void {
  // Toolbar
  input('files-search').addEventListener('input', () => {
    if (filesSearchTimer) clearTimeout(filesSearchTimer);
    filesSearchTimer = setTimeout(() => void loadFiles(), 250);
  });
  document.getElementById('files-type-filter')?.addEventListener('change', () => void loadFiles());
  document.getElementById('files-sort')?.addEventListener('change', () => void loadFiles());
  document.getElementById('files-unassigned-only')?.addEventListener('change', () => void loadFiles());

  // Upload picker
  document.getElementById('files-upload-btn')?.addEventListener('click', () => {
    input('files-file-input').click();
  });
  document.getElementById('files-file-input')?.addEventListener('change', (e) => {
    const target = e.target as HTMLInputElement;
    void uploadFiles(Array.from(target.files || []));
    target.value = '';
  });

  // Drag and drop
  const dropzone = document.getElementById('files-dropzone');
  if (dropzone) {
    for (const ev of ['dragenter', 'dragover'] as const) {
      dropzone.addEventListener(ev, (e) => {
        e.preventDefault();
        e.stopPropagation();
        dropzone.classList.add('dragover');
      });
    }
    for (const ev of ['dragleave', 'drop'] as const) {
      dropzone.addEventListener(ev, (e) => {
        e.preventDefault();
        e.stopPropagation();
        if (ev === 'dragleave' && e.target !== dropzone) return;
        dropzone.classList.remove('dragover');
      });
    }
    dropzone.addEventListener('drop', (e) => {
      const files = Array.from((e as DragEvent).dataTransfer?.files || []);
      if (files.length) void uploadFiles(files);
    });
    initCheckboxDrag(dropzone);
  }

  // Select all listed
  document.getElementById('files-select-all')?.addEventListener('change', (e) => {
    selectAllListed((e.target as HTMLInputElement).checked);
  });

  // Bulk bar
  document.getElementById('files-bulk-clear')?.addEventListener('click', () => {
    filesSelected.clear();
    renderFiles();
  });
  document.getElementById('files-bulk-delete')?.addEventListener('click', () => {
    if (!filesSelected.size) return;
    openDeleteModal(Array.from(filesSelected));
  });
  document.getElementById('files-bulk-assign')?.addEventListener('click', () => {
    if (!filesSelected.size) return;
    openAssignModal(Array.from(filesSelected));
  });

  // Modal close buttons
  document
    .getElementById('files-assign-close')
    ?.addEventListener('click', () => closeModal('files-assign-modal'));
  document
    .getElementById('files-assign-cancel')
    ?.addEventListener('click', () => closeModal('files-assign-modal'));
  document.getElementById('files-assign-save')?.addEventListener('click', saveAssign);

  document.getElementById('files-preview-close')?.addEventListener('click', closePreviewModal);
  document.getElementById('files-preview-dismiss')?.addEventListener('click', closePreviewModal);

  document
    .getElementById('files-delete-close')
    ?.addEventListener('click', () => closeModal('files-delete-modal'));
  document
    .getElementById('files-delete-cancel')
    ?.addEventListener('click', () => closeModal('files-delete-modal'));
  document.getElementById('files-delete-confirm')?.addEventListener('click', confirmDelete);

  document
    .getElementById('files-replace-close')
    ?.addEventListener('click', () => closeModal('files-replace-modal'));
  document
    .getElementById('files-replace-cancel')
    ?.addEventListener('click', () => closeModal('files-replace-modal'));
  document.getElementById('files-replace-input')?.addEventListener('change', (e) => {
    const file = (e.target as HTMLInputElement).files?.[0];
    const info = document.getElementById('files-replace-new');
    if (info) info.textContent = file ? `New: ${file.name} (${fmtBytes(file.size)})` : '';
  });
  document.getElementById('files-replace-confirm')?.addEventListener('click', confirmReplace);
}

// Dispatched from main.ts via the dropzone-scoped listener.
export function handleFilesAction(action: string, target: HTMLElement): void {
  const id = target.getAttribute('data-id') || '';
  switch (action) {
    case 'select': {
      const checkbox = target as HTMLInputElement;
      if (checkbox.checked) filesSelected.add(id);
      else filesSelected.delete(id);
      target.closest('.file-row')?.classList.toggle('selected', checkbox.checked);
      updateBulkBar();
      break;
    }
    case 'preview':
      openPreviewModal(id);
      break;
    case 'rename':
      startRename(target);
      break;
    case 'assign-one':
      openAssignModal([id]);
      break;
    case 'replace':
      openReplaceModal(id);
      break;
    case 'download':
      window.open(
        `/api/admin/files/${id}/download?token=${encodeURIComponent(getToken())}`,
        '_blank',
      );
      break;
    case 'delete-one':
      openDeleteModal([id]);
      break;
    case 'unassign': {
      const room = target.getAttribute('data-room') || '';
      void unassignFile(room, id);
      break;
    }
  }
}
