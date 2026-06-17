// Single-key shortcuts for the viewer toolbar (#159). Each key maps to a
// toolbar button id and simply dispatches its click, so all existing guards
// (disabled until live, hidden outside focus mode, presenter-only logic,
// WS broadcasts) are inherited for free.
import { pttPress, pttRelease } from './conference.js';
import { viewerStore } from './state.js';

const MIC_KEY = 's'; // microphone — hold-to-talk in push-to-talk mode (#188)

const KEY_TO_BTN: Record<string, string> = {
  a: 'cam-btn', // camera
  [MIC_KEY]: 'mic-btn', // microphone
  d: 'pointer-btn', // pointer (focus mode only)
  f: 'fullscreen-btn', // fullscreen
  m: 'mute-btn', // stream mute/unmute
  x: 'focus-btn', // focus view
  c: 'chat-toggle', // chat panel
  v: 'conf-toggle', // call strip (focus/pinned mode only)
};

// Whether the mic button is currently actionable (live + visible). A native
// disabled button ignores .click(); offsetParent === null means it's hidden
// (outside focus mode, or app not yet visible).
function btnActionable(id: string): boolean {
  const btn = document.getElementById(id) as HTMLButtonElement | null;
  return !!btn && !btn.disabled && btn.offsetParent !== null;
}

export function initShortcuts(): void {
  document.addEventListener('keydown', (e) => {
    // Leave browser/OS combos (copy, devtools, etc.) untouched.
    if (e.ctrlKey || e.metaKey || e.altKey) return;
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
