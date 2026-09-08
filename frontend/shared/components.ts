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

interface DialogOpts<T> {
  title: string;
  /** Markup between the header and the footer. */
  bodyHtml: string;
  /** Footer button markup. Elements carry `data-act="ok"` / `data-act="cancel"`. */
  footerHtml: string;
  /** Resolved on backdrop click, the ✕, Escape, or any `data-act="cancel"`. */
  dismissValue: T;
  /** Resolved when a `data-act="ok"` element is activated. */
  onOk(modal: HTMLDivElement): T;
  /**
   * Whether Enter acts as OK. False makes Enter a no-op, which is the point for
   * destructive confirms — Enter used to silently mean "yes" there.
   *
   * A boolean rather than an `onEnter` callback returning `T | undefined`: for
   * `T = void` the "no value" sentinel and a legitimate result are the same
   * `undefined`, so a callback would silently stop Enter working on notices.
   */
  enterActsAsOk: boolean;
  /** Runs once the dialog is in the DOM. */
  afterMount?(modal: HTMLDivElement): void;
}

/**
 * Shared skeleton for the three dynamic dialogs below: overlay + lifecycle +
 * key handling + teardown, all of which were written out three times.
 *
 * Keyboard focus is trapped inside the dialog while it is open and restored to
 * whatever had it before — without that, Tab walks the page behind a dialog
 * that is visually modal.
 */
function openDialog<T>(opts: DialogOpts<T>): Promise<T> {
  return new Promise((resolve) => {
    const previouslyFocused = document.activeElement as HTMLElement | null;

    const overlay = document.createElement('div');
    overlay.className = 'modal-overlay';
    overlay.style.display = 'flex';
    const modal = document.createElement('div');
    modal.className = 'modal';
    modal.style.width = '420px';
    modal.setAttribute('role', 'dialog');
    modal.setAttribute('aria-modal', 'true');
    overlay.appendChild(modal);

    modal.innerHTML = `
      <div class="modal-header">
        <span class="modal-title"></span>
        <button class="btn btn-sm" data-act="cancel" aria-label="Close">✕</button>
      </div>
      ${opts.bodyHtml}
      <div class="modal-footer">${opts.footerHtml}</div>`;
    (modal.querySelector('.modal-title') as HTMLElement).textContent = opts.title;

    const focusable = (): HTMLElement[] =>
      Array.from(
        modal.querySelectorAll<HTMLElement>('button, input, select, textarea, [href]'),
      ).filter((el) => !el.hasAttribute('disabled'));

    const done = (val: T): void => {
      document.removeEventListener('keydown', onKey, true);
      overlay.remove();
      previouslyFocused?.focus?.();
      resolve(val);
    };

    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') {
        e.preventDefault();
        done(opts.dismissValue);
        return;
      }
      if (e.key === 'Enter') {
        if (opts.enterActsAsOk) {
          e.preventDefault();
          done(opts.onOk(modal));
        }
        return;
      }
      if (e.key === 'Tab') {
        const items = focusable();
        if (items.length === 0) return;
        const first = items[0]!;
        const last = items[items.length - 1]!;
        const active = document.activeElement;
        if (e.shiftKey && (active === first || !modal.contains(active))) {
          e.preventDefault();
          last.focus();
        } else if (!e.shiftKey && active === last) {
          e.preventDefault();
          first.focus();
        }
      }
    };

    overlay.addEventListener('click', (e) => {
      const t = e.target as HTMLElement;
      if (t === overlay || t.closest('[data-act="cancel"]')) done(opts.dismissValue);
      else if (t.closest('[data-act="ok"]')) done(opts.onOk(modal));
    });
    // Capture phase, so the dialog sees Escape before page-level handlers do.
    document.addEventListener('keydown', onKey, true);
    document.body.appendChild(overlay);
    opts.afterMount?.(modal);
  });
}

/** Body paragraph shared by confirm and notice. */
const MESSAGE_P =
  '<p class="modal-message" style="margin:4px 0 18px;font-size:14px;line-height:1.5;color:var(--text);white-space:pre-line"></p>';

function setMessage(modal: HTMLElement, text: string | undefined, html: string | undefined): void {
  const el = modal.querySelector('.modal-message') as HTMLElement;
  if (html !== undefined) el.innerHTML = html;
  else el.textContent = text ?? '';
}

export function confirmModal(opts: ConfirmOpts): Promise<boolean> {
  return openDialog<boolean>({
    title: opts.title,
    bodyHtml: MESSAGE_P,
    footerHtml:
      `<button class="btn" data-act="cancel"></button>` +
      `<button class="btn ${opts.danger ? 'btn-danger' : 'btn-primary'}" data-act="ok"></button>`,
    dismissValue: false,
    onOk: () => true,
    // Enter confirms only a non-destructive action. On a danger dialog it used
    // to mean "yes" — so a stray Return while a delete confirm was open deleted
    // the thing. Destructive confirms must be clicked.
    enterActsAsOk: !opts.danger,
    afterMount: (modal) => {
      setMessage(modal, opts.message, opts.messageHtml);
      (modal.querySelector('[data-act="cancel"].btn:not(.btn-sm)') as HTMLElement).textContent =
        opts.cancelLabel ?? 'Cancel';
      (modal.querySelector('[data-act="ok"]') as HTMLElement).textContent =
        opts.confirmLabel ?? 'Confirm';
      // Focus the safe option, so Space/Enter on the focused control cancels.
      (modal.querySelector('[data-act="cancel"].btn:not(.btn-sm)') as HTMLElement).focus();
    },
  });
}

// Single-button informational dialog (vs. confirmModal's two-button confirm).
// Resolves when dismissed via the button, the ✕, the backdrop, Esc or Enter.
export function noticeModal(opts: NoticeOpts): Promise<void> {
  return openDialog<void>({
    title: opts.title,
    bodyHtml: MESSAGE_P,
    footerHtml: `<button class="btn btn-primary" data-act="ok"></button>`,
    dismissValue: undefined,
    onOk: () => undefined,
    // A notice has nothing to lose, so Enter dismisses it.
    enterActsAsOk: true,
    afterMount: (modal) => {
      setMessage(modal, opts.message, opts.messageHtml);
      const ok = modal.querySelector('[data-act="ok"].btn:not(.btn-sm)') as HTMLElement;
      ok.textContent = opts.buttonLabel ?? 'Got it';
      ok.focus();
    },
  });
}

export function promptModal(opts: PromptOpts): Promise<string | null> {
  const read = (modal: HTMLElement): string | null => {
    const v = (modal.querySelector('input') as HTMLInputElement).value.trim();
    return v ? v : null;
  };
  return openDialog<string | null>({
    title: opts.title,
    bodyHtml:
      `<p class="prompt-msg" style="margin:4px 0 14px;font-size:14px;line-height:1.5;color:var(--text)"></p>` +
      `<div class="form-row"><label></label><input type="${opts.inputType ?? 'text'}"></div>`,
    footerHtml:
      `<button class="btn" data-act="cancel">Cancel</button>` +
      `<button class="btn btn-primary" data-act="ok"></button>`,
    dismissValue: null,
    onOk: read,
    // Enter submits via the same `read` the OK button uses, so an empty field
    // resolves null exactly as clicking Confirm would.
    enterActsAsOk: true,
    afterMount: (modal) => {
      const msgEl = modal.querySelector('.prompt-msg') as HTMLElement;
      if (opts.message) msgEl.textContent = opts.message;
      else msgEl.remove();
      (modal.querySelector('label') as HTMLElement).textContent = opts.label;
      const input = modal.querySelector('input') as HTMLInputElement;
      if (opts.placeholder) input.placeholder = opts.placeholder;
      (modal.querySelector('[data-act="ok"]') as HTMLElement).textContent =
        opts.confirmLabel ?? 'Confirm';
      input.focus();
    },
  });
}
