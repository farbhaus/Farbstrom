import { applyBranding } from '../shared/branding.js';
import { initPrivacyNotice } from '../shared/privacy-notice.js';

function goToRoom(): void {
  const input = document.getElementById('landing-input') as HTMLInputElement;
  const errEl = document.getElementById('landing-error');
  const raw = input.value.trim();
  if (errEl) errEl.textContent = '';
  if (!raw) {
    if (errEl) errEl.textContent = 'Please enter a room URL.';
    return;
  }
  let dest: string | undefined;
  try {
    const url = raw.startsWith('http')
      ? new URL(raw)
      : new URL('https://x/' + raw.replace(/^\/+/, ''));
    const parts = url.pathname.replace(/^\//, '').split('/').filter(Boolean);
    dest = parts[parts.length - 1];
  } catch {
    dest = raw.replace(/^\/+|\/+$/g, '').split('/').pop();
  }
  if (!dest) {
    if (errEl) errEl.textContent = 'Could not parse a room from that URL.';
    return;
  }
  location.href = '/watch/' + dest + location.search;
}

document.getElementById('landing-btn')?.addEventListener('click', goToRoom);
document.getElementById('landing-input')?.addEventListener('keydown', (e) => {
  if ((e as KeyboardEvent).key === 'Enter') goToRoom();
});

// Both the logo and the wordmark start hidden so neither flashes before
// /api/branding resolves; reveal the wordmark only when there's no custom
// logo (or the fetch fails — applyBranding returns null).
void applyBranding({
  logoEl: document.getElementById('brand-logo') as HTMLImageElement | null,
  setTitle: true,
}).then((data) => {
  if (!data?.hasLogo) {
    document.getElementById('brand-fallback')?.classList.remove('u-hidden');
  }
});

initPrivacyNotice();
