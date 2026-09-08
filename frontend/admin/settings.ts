// Settings tab: change the admin password, enrol/disable TOTP 2FA, and
// register/remove WebAuthn passkeys. Mirrors branding.ts's load/init split.

import { apiFetch, setToken } from './auth.js';
import { toast, esc, fmtDateTime } from '../shared/utils.js';
import { closeModal, confirmModal, openModal, promptModal } from '../shared/components.js';
import { doRegister, webauthnSupported } from './webauthn.js';
import type { SettingsStatus } from './types.js';

function $(id: string): HTMLElement | null {
  return document.getElementById(id);
}
function val(id: string): string {
  return ($(id) as HTMLInputElement | null)?.value ?? '';
}

function show(id: string, visible: boolean): void {
  const el = $(id);
  if (el) el.style.display = visible ? '' : 'none';
}

export async function loadSettings(): Promise<void> {
  // Independent of the account-settings status call below, so it still loads if
  // that fails.
  void loadSrtEncryption();
  let res: Response | null;
  try {
    res = await apiFetch('/api/admin/settings/status');
  } catch (e) {
    console.error('[settings] status request failed', e);
    toast('Could not load settings');
    return;
  }
  if (!res || !res.ok) {
    console.error('[settings] status returned', res?.status);
    return;
  }
  const s: SettingsStatus = await res.json();

  const pwState = $('set-pw-state');
  if (pwState)
    pwState.textContent = s.passwordIsCustom
      ? 'A custom password is set.'
      : 'Using the password from the server environment.';

  const totpState = $('set-totp-state');
  if (totpState)
    totpState.textContent = s.totpEnabled
      ? 'Two-factor authentication is ON.'
      : 'Two-factor authentication is OFF.';
  // Null-safe: a single missing element must never abort the whole render.
  show('set-totp-enable-btn', !s.totpEnabled);
  show('set-totp-disable-btn', s.totpEnabled);

  const list = $('set-pk-list');
  if (list) {
    list.innerHTML = s.passkeys.length
      ? s.passkeys
          .map(
            (p) => `
        <div class="room-card-header">
          <div class="room-card-info">
            <div class="room-card-name">${esc(p.label)}</div>
            <div class="room-card-meta">Added ${esc(fmtDateTime(p.created_at) || '')}${
              p.last_used_at ? ' · Last used ' + esc(fmtDateTime(p.last_used_at)) : ''
            }</div>
          </div>
          <div class="room-card-actions">
            <button class="btn btn-sm btn-danger" data-pk-del="${esc(p.id)}">Remove</button>
          </div>
        </div>`,
          )
          .join('')
      : '<span style="font-size:13px;color:var(--dim)">No passkeys registered.</span>';
  }
}

async function changePassword(): Promise<void> {
  const current = val('set-pw-current');
  const next = val('set-pw-new');
  if (next !== val('set-pw-confirm')) {
    toast('New passwords do not match');
    return;
  }
  if (next.length < 12) {
    toast('New password must be at least 12 characters');
    return;
  }
  const res = await apiFetch('/api/admin/settings/password', {
    method: 'POST',
    body: JSON.stringify({ current, new: next }),
  });
  if (res && res.ok) {
    (['set-pw-current', 'set-pw-new', 'set-pw-confirm'] as const).forEach(
      (id) => (($(id) as HTMLInputElement).value = ''),
    );
    // Changing the password revokes every session minted before it — including
    // this one. The server hands back a replacement stamped with the new
    // generation; adopt it or the next request from this tab 401s.
    await adoptReissuedToken(res);
    toast('Password changed — other devices signed out');
    void loadSettings();
  } else {
    const e = res ? await res.json().catch(() => ({})) : {};
    toast(e.error || 'Password change failed');
  }
}

async function startTotpSetup(): Promise<void> {
  const res = await apiFetch('/api/admin/settings/totp/setup', { method: 'POST' });
  if (!res || !res.ok) {
    // Refused while already enrolled — re-running setup would rotate the secret
    // and switch 2FA off, so the server sends back what to do instead.
    const e = res ? await res.json().catch(() => ({})) : {};
    toast(e.error || 'Could not start setup');
    return;
  }
  const d = await res.json();
  ($('set-totp-qr') as HTMLImageElement).src = d.qr;
  ($('set-totp-secret') as HTMLElement).textContent = d.secret;
  ($('set-totp-setup') as HTMLElement).style.display = '';
}

async function confirmTotp(): Promise<void> {
  const code = val('set-totp-code').trim();
  const res = await apiFetch('/api/admin/settings/totp/enable', {
    method: 'POST',
    body: JSON.stringify({ code }),
  });
  if (!res || !res.ok) {
    const e = res ? await res.json().catch(() => ({})) : {};
    toast(e.error || 'Incorrect code');
    return;
  }
  const d = await res.json();
  ($('set-totp-setup') as HTMLElement).style.display = 'none';
  ($('set-totp-code') as HTMLInputElement).value = '';
  const box = $('set-totp-recovery');
  if (box) {
    box.style.display = '';
    box.innerHTML =
      '<div class="room-card-meta">Save these one-time recovery codes somewhere safe — they are shown only once:</div><pre style="font-size:13px;line-height:1.7">' +
      (d.recoveryCodes as string[]).map(esc).join('\n') +
      '</pre>';
  }
  toast('Two-factor enabled');
  void loadSettings();
}

// Teardown needs both factors: the password, then a current authenticator code
// (or a recovery code, for the case where the authenticator is what was lost).
// The server enforces this — asking for the password alone gets a 403, which
// reports the reason rather than signing you out the way a 401 would.
async function disableTotp(): Promise<void> {
  const password = await promptModal({
    title: 'Disable Two-Factor',
    message: 'Confirm your password to turn off TOTP 2FA.',
    label: 'Password',
    inputType: 'password',
    confirmLabel: 'Continue',
  });
  if (!password) return;

  const code = await promptModal({
    title: 'Disable Two-Factor',
    message: 'Enter a code from your authenticator app, or one of your recovery codes.',
    label: 'Code',
    confirmLabel: 'Disable 2FA',
  });
  if (!code) return;

  const res = await apiFetch('/api/admin/settings/totp/disable', {
    method: 'POST',
    body: JSON.stringify({ password, code }),
  });
  if (res && res.ok) {
    const box = $('set-totp-recovery');
    if (box) box.style.display = 'none';
    toast('Two-factor disabled');
    void loadSettings();
  } else {
    const e = res ? await res.json().catch(() => ({})) : {};
    toast(e.error || 'Could not disable 2FA');
  }
}

/// Take the replacement token an endpoint hands back after revoking sessions.
/// Without this the tab that performed the revocation would be logged out by
/// its own action.
async function adoptReissuedToken(res: Response): Promise<void> {
  const data = await res.json().catch(() => ({}));
  if (typeof data.token === 'string' && data.token) setToken(data.token);
}

/// Revoke every other admin session — including trusted browsers, which is the
/// thing that makes a 90-day "trust this browser" token safe to hand out.
/// Password-gated server-side, so ask for it here.
async function signOutEverywhere(): Promise<void> {
  const ok = await confirmModal({
    title: 'Sign out other devices',
    message:
      'Every other signed-in browser will be logged out immediately, including any you ticked "trust this browser" on. This session stays active.',
    confirmLabel: 'Sign out others',
    danger: true,
  });
  if (!ok) return;

  const password = await promptModal({
    title: 'Sign out other devices',
    message: 'Confirm your password.',
    label: 'Password',
    inputType: 'password',
    confirmLabel: 'Sign out others',
  });
  if (!password) return;

  const res = await apiFetch('/api/admin/settings/sign-out-everywhere', {
    method: 'POST',
    body: JSON.stringify({ password }),
  });
  if (res && res.ok) {
    await adoptReissuedToken(res);
    toast('Other devices signed out');
  } else {
    const e = res ? await res.json().catch(() => ({})) : {};
    toast(e.error || 'Could not sign out other devices');
  }
}

function openPasskeyModal(): void {
  if (!webauthnSupported()) {
    toast('This browser does not support passkeys');
    return;
  }
  const input = $('passkey-name') as HTMLInputElement | null;
  if (input) input.value = '';
  openModal('passkey-modal');
  input?.focus();
}

async function addPasskey(): Promise<void> {
  const label = ($('passkey-name') as HTMLInputElement | null)?.value.trim() ?? '';
  if (!label) {
    toast('Name required');
    return;
  }
  closeModal('passkey-modal');
  const start = await apiFetch('/api/admin/settings/passkeys/register/start', {
    method: 'POST',
    body: JSON.stringify({ label }),
  });
  if (!start || !start.ok) {
    toast('Could not start registration');
    return;
  }
  const { id, options } = await start.json();
  let credential: unknown;
  try {
    credential = await doRegister(options);
  } catch {
    toast('Passkey registration cancelled');
    return;
  }
  const fin = await apiFetch('/api/admin/settings/passkeys/register/finish', {
    method: 'POST',
    body: JSON.stringify({ id, label, credential }),
  });
  if (fin && fin.ok) {
    toast('Passkey added');
    void loadSettings();
  } else {
    const e = fin ? await fin.json().catch(() => ({})) : {};
    toast(e.error || 'Registration failed');
  }
}

async function deletePasskey(id: string): Promise<void> {
  if (
    !(await confirmModal({
      title: 'Remove Passkey',
      message: 'This passkey will no longer be able to sign in.',
      confirmLabel: 'Remove',
      danger: true,
    }))
  )
    return;
  const res = await apiFetch(`/api/admin/settings/passkeys/${id}`, { method: 'DELETE' });
  if (res && res.ok) {
    toast('Passkey removed');
    void loadSettings();
  } else {
    toast('Remove failed');
  }
}

// --- SRT wire encryption (gh #208) ---------------------------------------
// Two independent legs; the checkboxes stage a desired state and Apply commits
// both at once so OME restarts only once.

interface SrtEncState {
  ingestEnabled: boolean;
  playbackEnabled: boolean;
}

let srtSaved: SrtEncState = { ingestEnabled: false, playbackEnabled: false };

function srtChecks(): SrtEncState {
  return {
    ingestEnabled: !!($('srt-enc-ingest') as HTMLInputElement | null)?.checked,
    playbackEnabled: !!($('srt-enc-playback') as HTMLInputElement | null)?.checked,
  };
}

function syncSrtCheckboxes(): void {
  const ing = $('srt-enc-ingest') as HTMLInputElement | null;
  const pb = $('srt-enc-playback') as HTMLInputElement | null;
  if (ing) ing.checked = srtSaved.ingestEnabled;
  if (pb) pb.checked = srtSaved.playbackEnabled;
}

// Enable Apply only when the checkboxes differ from the persisted state.
function updateSrtApplyState(): void {
  const cur = srtChecks();
  const dirty =
    cur.ingestEnabled !== srtSaved.ingestEnabled ||
    cur.playbackEnabled !== srtSaved.playbackEnabled;
  const btn = $('srt-enc-apply') as HTMLButtonElement | null;
  if (btn) btn.disabled = !dirty;
}

async function loadSrtEncryption(): Promise<void> {
  const res = await apiFetch('/api/stream-keys/srt-config');
  if (!res || !res.ok) return;
  const c = await res.json().catch(() => null);
  if (!c) return;
  srtSaved = { ingestEnabled: !!c.ingestEnabled, playbackEnabled: !!c.playbackEnabled };
  syncSrtCheckboxes();
  updateSrtApplyState();
}

async function applySrtEncryption(): Promise<void> {
  const cur = srtChecks();
  const confirmed = await confirmModal({
    title: 'Apply SRT encryption',
    message:
      'This restarts the streaming engine to apply the change. All live streams drop for a few seconds, and affected encoders and players must reconnect with the new passphrase.\n\nContinue?',
    confirmLabel: 'Apply & restart',
    danger: true,
  });
  if (!confirmed) return;
  const btn = $('srt-enc-apply') as HTMLButtonElement | null;
  if (btn) btn.disabled = true;
  const res = await apiFetch('/api/stream-keys/srt-encryption', {
    method: 'POST',
    body: JSON.stringify({ ingest: cur.ingestEnabled, playback: cur.playbackEnabled }),
  });
  if (res && res.ok) {
    const c = await res.json().catch(() => null);
    if (c) srtSaved = { ingestEnabled: !!c.ingestEnabled, playbackEnabled: !!c.playbackEnabled };
    syncSrtCheckboxes();
    toast('SRT encryption updated — engine restarting');
  } else {
    toast('Failed to update SRT encryption');
  }
  updateSrtApplyState(); // re-enables Apply if the POST failed (still dirty)
}

export function initSettings(): void {
  $('set-pw-btn')?.addEventListener('click', changePassword);
  $('srt-enc-ingest')?.addEventListener('change', updateSrtApplyState);
  $('srt-enc-playback')?.addEventListener('change', updateSrtApplyState);
  $('srt-enc-apply')?.addEventListener('click', () => void applySrtEncryption());
  $('set-totp-enable-btn')?.addEventListener('click', startTotpSetup);
  $('set-signout-all-btn')?.addEventListener('click', signOutEverywhere);
  $('set-totp-confirm-btn')?.addEventListener('click', confirmTotp);
  $('set-totp-disable-btn')?.addEventListener('click', disableTotp);
  $('set-pk-add-btn')?.addEventListener('click', openPasskeyModal);
  $('passkey-modal-save')?.addEventListener('click', addPasskey);
  $('passkey-modal-close')?.addEventListener('click', () => closeModal('passkey-modal'));
  $('passkey-modal-cancel')?.addEventListener('click', () => closeModal('passkey-modal'));
  $('passkey-name')?.addEventListener('keydown', (e) => {
    if ((e as KeyboardEvent).key === 'Enter') void addPasskey();
  });
  $('set-pk-list')?.addEventListener('click', (e) => {
    const btn = (e.target as HTMLElement).closest<HTMLElement>('[data-pk-del]');
    if (btn) void deletePasskey(btn.getAttribute('data-pk-del') || '');
  });
}
