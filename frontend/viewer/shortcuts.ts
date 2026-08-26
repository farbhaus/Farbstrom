// Single-key shortcuts for the viewer toolbar (#159), and the ? sheet that
// lists them (#230). Each key maps to a toolbar button id and simply dispatches
// its click, so all existing guards (disabled until live, hidden outside focus
// mode, presenter-only logic, WS broadcasts) are inherited for free.
import { confirmModal } from '../shared/components.js';
import { pttPress, pttRelease } from './conference.js';
import { viewerStore } from './state.js';
import { isTourActive, startTour } from './tour.js';

const MIC_KEY = 's'; // microphone — hold-to-talk in push-to-talk mode (#188)

// The one source of truth for the keys: both the handler below and the ? sheet
// read this, so a shortcut can't be added without also being documented.
// Order is the order the sheet lists them in.
const SHORTCUTS: { key: string; btn: string; label: string }[] = [
  { key: 'a', btn: 'cam-btn', label: 'Camera' },
  { key: MIC_KEY, btn: 'mic-btn', label: 'Microphone (hold, in push-to-talk)' },
  { key: 'd', btn: 'pointer-btn', label: 'Pointer (focus view only)' },
  { key: 'x', btn: 'focus-btn', label: 'Focus view' },
  { key: 'v', btn: 'conf-toggle', label: 'Participant strip (focus view only)' },
  { key: 'c', btn: 'chat-toggle', label: 'Chat' },
  { key: 'w', btn: 'scopes-btn', label: 'Scopes' },
  { key: 'm', btn: 'mute-btn', label: 'Mute the stream' },
  { key: 'f', btn: 'fullscreen-btn', label: 'Fullscreen' },
];

const KEY_TO_BTN: Record<string, string> = Object.fromEntries(
  SHORTCUTS.map((s) => [s.key, s.btn]),
);

// Whether the mic button is currently actionable (live + visible). A native
// disabled button ignores .click(); offsetParent === null means it's hidden
// (outside focus mode, or app not yet visible).
function btnActionable(id: string): boolean {
  const btn = document.getElementById(id) as HTMLButtonElement | null;
  return !!btn && !btn.disabled && btn.offsetParent !== null;
}

// The ? button. Lists the keys and offers the room tour — the tour is the long
// way round, so it's the sheet's suggestion rather than what ? does on its own.
let helpOpen = false;

async function openHelp(): Promise<void> {
  if (helpOpen || isTourActive()) return;
  helpOpen = true;
  const rows = SHORTCUTS.map(
    (s) =>
      `<span class="key-cap">${s.key.toUpperCase()}</span><span>${s.label}</span>`,
  ).join('');
  const take = await confirmModal({
    title: 'Shortcuts',
    message: '',
    messageHtml:
      `<span class="shortcut-list">${rows}</span>` +
      '<span class="shortcut-note">Keys are off while you are typing in chat.</span>' +
      '<span class="shortcut-note">New to the room? The tour walks through the controls.</span>',
    confirmLabel: 'Take the tour',
    cancelLabel: 'Close',
  });
  helpOpen = false;
  if (take) startTour();
}

export function initShortcuts(): void {
  document.getElementById('help-btn')?.addEventListener('click', () => void openHelp());

  document.addEventListener('keydown', (e) => {
    // Leave browser/OS combos (copy, devtools, etc.) untouched.
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    // The tour (#230) owns the keyboard while it runs — it describes these
    // controls, so firing them underneath it would be the one thing it promises
    // not to do. Same for the sheet that lists them. Esc/arrows: tour.ts.
    if (isTourActive() || helpOpen) return;
    // Don't hijack keys while the user is typing.
    const t = e.target as HTMLElement | null;
    if (t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName))) return;
    const key = e.key.toLowerCase();
    // Push-to-talk: the mic key becomes hold-to-talk. keydown auto-repeats while
    // held, so guard on e.repeat; keyup (below) re-mutes.
    if (key === MIC_KEY && viewerStore.get().pttEnabled) {
      if (!btnActionable('mic-btn')) return;
      e.preventDefault();
      if (!e.repeat) void pttPress();
      return;
    }
    const id = KEY_TO_BTN[key];
    if (!id || !btnActionable(id)) return;
    e.preventDefault();
    (document.getElementById(id) as HTMLButtonElement).click();
  });
  // Mic-key release re-mutes in PTT mode. pttRelease() no-ops when not held, so
  // it's safe to fire unconditionally (typing context, non-PTT mode, etc.).
  document.addEventListener('keyup', (e) => {
    if (e.key.toLowerCase() === MIC_KEY) void pttRelease();
  });
}
