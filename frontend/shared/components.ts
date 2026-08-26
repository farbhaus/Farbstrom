// Modal helpers — every admin modal is opened/closed by toggling display
// on a `.modal-overlay` element. These wrap the boilerplate.

export function openModal(id: string): void {
  const el = document.getElementById(id);
  if (el) el.style.display = 'flex';
}

export function closeModal(id: string): void {
  const el = document.getElementById(id);
  if (el) el.style.display = 'none';
}

// Wire a Cancel/Close pair (or any button list) to close the given modal.
// Each entry is the button id; clicking it hides the modal.
export function wireModalClose(modalId: string, buttonIds: string[]): void {
  for (const bid of buttonIds) {
    const btn = document.getElementById(bid);
    if (btn) btn.addEventListener('click', () => closeModal(modalId));
  }
}

// ---- Dynamic modals --------------------------------------------------------
// confirmModal / promptModal build a modal in the site's design language on
// the fly and resolve a Promise, replacing native confirm()/prompt() so every
// dialog matches the rest of the admin UI. The modal is removed on close.

interface ConfirmOpts {
  title: string;
  // Plain-text body (escaped). Use messageHtml instead for trusted markup.
  message: string;
  // Trusted HTML body (caller-controlled, never user input) — wins over message.
  messageHtml?: string;
  confirmLabel?: string;
  cancelLabel?: string;
  danger?: boolean;
}

interface NoticeOpts {
  title: string;
  // Plain-text body (escaped). Use messageHtml instead for trusted markup.
  message?: string;
  // Trusted HTML body (caller-controlled, never user input) — wins over message.
  messageHtml?: string;
  buttonLabel?: string;
}

interface PromptOpts {
  title: string;
  message?: string;
  label: string;
  confirmLabel?: string;
  inputType?: 'text' | 'password';
  placeholder?: string;
}

function buildOverlay(): { overlay: HTMLDivElement; modal: HTMLDivElement } {
  const overlay = document.createElement('div');
  overlay.className = 'modal-overlay';
  overlay.style.display = 'flex';
  const modal = document.createElement('div');
  modal.className = 'modal';
  modal.style.width = '420px';
  overlay.appendChild(modal);
  return { overlay, modal };
}

export function confirmModal(opts: ConfirmOpts): Promise<boolean> {
  return new Promise((resolve) => {
    const { overlay, modal } = buildOverlay();
    modal.innerHTML = `
      <div class="modal-header">
        <span class="modal-title"></span>
        <button class="btn btn-sm" data-act="cancel">✕</button>
      </div>
      <p style="margin:4px 0 18px;font-size:14px;line-height:1.5;color:var(--text);white-space:pre-line"></p>
      <div class="modal-footer">
        <button class="btn" data-act="cancel"></button>
        <button class="btn ${opts.danger ? 'btn-danger' : 'btn-primary'}" data-act="ok"></button>
      </div>`;
    (modal.querySelector('.modal-title') as HTMLElement).textContent = opts.title;
    const cBody = modal.querySelector('p') as HTMLElement;
    if (opts.messageHtml !== undefined) cBody.innerHTML = opts.messageHtml;
    else cBody.textContent = opts.message;
    (modal.querySelector('[data-act="cancel"].btn:not(.btn-sm)') as HTMLElement).textContent =
      opts.cancelLabel ?? 'Cancel';
    (modal.querySelector('[data-act="ok"]') as HTMLElement).textContent =
      opts.confirmLabel ?? 'Confirm';

    const done = (val: boolean): void => {
      document.removeEventListener('keydown', onKey);
      overlay.remove();
      resolve(val);
    };
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') done(false);
      if (e.key === 'Enter') done(true);
    };
    overlay.addEventListener('click', (e) => {
      const t = e.target as HTMLElement;
      if (t === overlay || t.closest('[data-act="cancel"]')) done(false);
      else if (t.closest('[data-act="ok"]')) done(true);
    });
    document.addEventListener('keydown', onKey);
    document.body.appendChild(overlay);
  });
}

// Single-button informational dialog (vs. confirmModal's two-button confirm).
// Resolves when dismissed via the button, the ✕, the backdrop, Esc or Enter.
export function noticeModal(opts: NoticeOpts): Promise<void> {
  return new Promise((resolve) => {
    const { overlay, modal } = buildOverlay();
    modal.innerHTML = `
      <div class="modal-header">
        <span class="modal-title"></span>
        <button class="btn btn-sm" data-act="ok">✕</button>
      </div>
      <p style="margin:4px 0 18px;font-size:14px;line-height:1.5;color:var(--text);white-space:pre-line"></p>
      <div class="modal-footer">
        <button class="btn btn-primary" data-act="ok"></button>
      </div>`;
    (modal.querySelector('.modal-title') as HTMLElement).textContent = opts.title;
    const body = modal.querySelector('p') as HTMLElement;
    if (opts.messageHtml !== undefined) body.innerHTML = opts.messageHtml;
    else body.textContent = opts.message ?? '';
    (modal.querySelector('[data-act="ok"].btn:not(.btn-sm)') as HTMLElement).textContent =
      opts.buttonLabel ?? 'Got it';

    const done = (): void => {
      document.removeEventListener('keydown', onKey);
      overlay.remove();
      resolve();
    };
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape' || e.key === 'Enter') done();
    };
    overlay.addEventListener('click', (e) => {
      const t = e.target as HTMLElement;
      if (t === overlay || t.closest('[data-act="ok"]')) done();
    });
    document.addEventListener('keydown', onKey);
    document.body.appendChild(overlay);
  });
}

export function promptModal(opts: PromptOpts): Promise<string | null> {
  return new Promise((resolve) => {
    const { overlay, modal } = buildOverlay();
    modal.innerHTML = `
      <div class="modal-header">
        <span class="modal-title"></span>
        <button class="btn btn-sm" data-act="cancel">✕</button>
      </div>
      <p class="prompt-msg" style="margin:4px 0 14px;font-size:14px;line-height:1.5;color:var(--text)"></p>
      <div class="form-row">
        <label></label>
        <input type="${opts.inputType ?? 'text'}">
      </div>
      <div class="modal-footer">
        <button class="btn" data-act="cancel">Cancel</button>
        <button class="btn btn-primary" data-act="ok"></button>
      </div>`;
    (modal.querySelector('.modal-title') as HTMLElement).textContent = opts.title;
    const msgEl = modal.querySelector('.prompt-msg') as HTMLElement;
    if (opts.message) msgEl.textContent = opts.message;
    else msgEl.remove();
    (modal.querySelector('label') as HTMLElement).textContent = opts.label;
    const input = modal.querySelector('input') as HTMLInputElement;
    if (opts.placeholder) input.placeholder = opts.placeholder;
    (modal.querySelector('[data-act="ok"]') as HTMLElement).textContent =
      opts.confirmLabel ?? 'Confirm';

    const done = (val: string | null): void => {
      document.removeEventListener('keydown', onKey);
      overlay.remove();
      resolve(val);
    };
    const submit = (): void => {
      const v = input.value.trim();
      done(v ? v : null);
    };
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') done(null);
      if (e.key === 'Enter') submit();
    };
    overlay.addEventListener('click', (e) => {
      const t = e.target as HTMLElement;
      if (t === overlay || t.closest('[data-act="cancel"]')) done(null);
      else if (t.closest('[data-act="ok"]')) submit();
    });
    document.addEventListener('keydown', onKey);
    document.body.appendChild(overlay);
    input.focus();
  });
}
