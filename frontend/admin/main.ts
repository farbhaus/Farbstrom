import {
  apiFetch,
  clearToken,
  fetchAuthMethods,
  getToken,
  login,
  passkeyLogin,
  setLogoutHandler,
} from './auth.js';
import { doAuthenticate } from './webauthn.js';
import {
  applyBrandingColorsOnce,
  applyLoginLogoOnce,
  initBranding,
  loadBranding,
} from './branding.js';
import {
  configureDashboard,
  handleDashboardAction,
  initDashboard,
  loadDashboard,
  loadOme,
  renderOmeIfReady,
  startDashboardTicker,
  stopDashboardTicker,
} from './dashboard.js';
import {
  configureFiles,
  handleFilesAction,
  initFiles,
  loadFiles,
  loadStorageStats,
} from './files.js';
import {
  getRooms,
  handleRoomAction,
  initRooms,
  loadRooms,
  refreshParticipantLists,
  setOnChange as setRoomsOnChange,
  setStreamKeys,
} from './rooms.js';
import {
  getStreamKeys,
  handleKeyAction,
  initStreamKeys,
  loadKeys,
  setOnChange as setKeysOnChange,
} from './stream-keys.js';
import { initSettings, loadSettings } from './settings.js';
import { copyToClipboard } from '../shared/utils.js';
import type { TabId } from './types.js';

let activeTab: TabId = 'rooms';

function getActiveTab(): TabId {
  return activeTab;
}

async function loadAll(): Promise<void> {
  await Promise.all([loadRooms(), loadKeys(), loadOme(), loadBranding(), loadDashboard()]);
  // After keys load, push them to rooms.ts so the room modal's <select> is populated.
  setStreamKeys(getStreamKeys());
}

function showApp(): void {
  setEl('login-screen', 'none');
  setEl('main-nav', 'flex');
  setEl('brand-corner', 'flex');
  setEl('logout-btn', 'inline-flex');
  setEl('main-content', 'block');
  void loadAll();
}

function doLogout(): void {
  clearToken();
  setEl('login-screen', 'flex');
  setEl('main-nav', 'none');
  setEl('brand-corner', 'none');
  setEl('logout-btn', 'none');
  setEl('main-content', 'none');
}

function setEl(id: string, display: string): void {
  const el = document.getElementById(id);
  if (el) el.style.display = display;
}

function switchTab(tab: TabId): void {
  document.querySelectorAll<HTMLButtonElement>('.btn-tab').forEach((b) => b.classList.remove('active'));
  document.querySelector<HTMLButtonElement>(`.btn-tab[data-tab="${tab}"]`)?.classList.add('active');
  activeTab = tab;

  // Use 'block' (not '') so inline style wins over the .u-hidden class applied in markup.
  setEl('tab-rooms', tab === 'rooms' ? 'block' : 'none');
  setEl('tab-keys', tab === 'keys' ? 'block' : 'none');
  setEl('tab-ome', tab === 'ome' ? 'block' : 'none');
  setEl('tab-branding', tab === 'branding' ? 'block' : 'none');
  setEl('tab-files', tab === 'files' ? 'block' : 'none');
  setEl('tab-settings', tab === 'settings' ? 'block' : 'none');

  if (tab === 'ome') {
    renderOmeIfReady();
    void loadDashboard();
    startDashboardTicker();
  } else {
    stopDashboardTicker();
  }
  if (tab === 'branding') void loadBranding();
  if (tab === 'files') {
    void loadFiles();
    void loadStorageStats();
  }
  if (tab === 'settings') void loadSettings();
}

function initLoginForm(): void {
  const totpRow = document.getElementById('totp-row');
  const totpInput = document.getElementById('totp-input') as HTMLInputElement | null;

  document.getElementById('login-form')?.addEventListener('submit', async (e) => {
    e.preventDefault();
    const errEl = document.getElementById('login-error');
    if (errEl) errEl.textContent = '';
    const passwordInput = document.getElementById('password-input') as HTMLInputElement;
    const result = await login(passwordInput.value, totpInput?.value || undefined);
    if (result.totpRequired) {
      if (totpRow) totpRow.style.display = '';
      totpInput?.focus();
      if (errEl) errEl.textContent = 'Enter your authenticator or recovery code';
      return;
    }
    if (!result.ok) {
      if (errEl) errEl.textContent = result.error || 'Sign in failed';
      return;
    }
    passwordInput.value = '';
    if (totpInput) totpInput.value = '';
    showApp();
  });

  document.getElementById('passkey-btn')?.addEventListener('click', async () => {
    const errEl = document.getElementById('login-error');
    if (errEl) errEl.textContent = '';
    const result = await passkeyLogin(doAuthenticate);
    if (!result.ok) {
      if (errEl) errEl.textContent = result.error || 'Passkey sign in failed';
      return;
    }
    showApp();
  });

  // Reveal the passkey button only if a passkey is registered.
  void fetchAuthMethods().then((m) => {
    if (m.passkeyEnabled) {
      const btn = document.getElementById('passkey-btn');
      if (btn) btn.style.display = '';
    }
  });

  document.getElementById('logout-btn')?.addEventListener('click', doLogout);
}

function initTabs(): void {
  document.querySelectorAll<HTMLButtonElement>('.btn-tab').forEach((btn) => {
    btn.addEventListener('click', () => {
      const tab = (btn.dataset.tab || 'rooms') as TabId;
      switchTab(tab);
    });
  });
}

function initDelegatedClicks(): void {
  // Click-to-copy for any readonly url-input field.
  document.addEventListener('click', (e) => {
    const el = (e.target as HTMLElement).closest<HTMLInputElement>('input.url-input');
    if (!el || !el.readOnly) return;
    copyToClipboard(el.value);
  });

  // Single delegated handler dispatches all data-action clicks to the right module.
  document.addEventListener('click', (e) => {
    const target = (e.target as HTMLElement).closest<HTMLElement>('[data-action]');
    if (!target) return;
    const action = target.getAttribute('data-action');
    if (!action) return;

    // Files dropzone delegates everything inside files area.
    if (target.closest('#files-dropzone')) {
      handleFilesAction(action, target);
      return;
    }

    switch (action) {
      case 'copy':
        copyToClipboard(target.getAttribute('data-value') || '');
        return;
      case 'delete-key':
        handleKeyAction(action, target);
        return;
      case 'kick-stream':
        handleDashboardAction(action, target);
        return;
      case 'edit-room':
      case 'delete-room':
      case 'reactivate-room':
      case 'enter-presenter':
      case 'rotate-host-key':
      case 'admit-all':
      case 'admit-one':
      case 'unkick-one':
        handleRoomAction(action, target);
        return;
    }
  });
}

function init(): void {
  applyBrandingColorsOnce();
  applyLoginLogoOnce();

  setLogoutHandler(doLogout);
  configureDashboard({
    getActiveTab,
    onStreamKicked: () => void loadRooms(),
  });
  configureFiles({ getRooms });

  setRoomsOnChange(() => void loadAll());
  setKeysOnChange(() => void loadAll());

  initLoginForm();
  initTabs();
  initRooms();
  initStreamKeys();
  initBranding();
  initDashboard();
  initFiles();
  initSettings();
  initDelegatedClicks();

  // Auto-restore session if a token is still valid.
  const token = getToken();
  if (token) {
    fetch('/api/rooms', { headers: { Authorization: `Bearer ${token}` } })
      .then((r) => (r.ok ? showApp() : doLogout()))
      .catch(() => doLogout());
  }

  // Background refresh: full reload every 15s, fast list poll every 3s.
  setInterval(() => {
    if (getToken()) void loadAll();
  }, 15000);
  setInterval(() => {
    if (getToken()) void refreshParticipantLists();
  }, 3000);
}

init();
