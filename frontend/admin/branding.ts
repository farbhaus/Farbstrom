import { apiFetch, getToken } from './auth.js';
import { confirmModal } from '../shared/components.js';
import { toast } from '../shared/utils.js';
import type { BrandingResponse } from './types.js';

const COLOR_FIELDS = ['accent', 'bg', 'surface', 'text', 'danger', 'green'] as const;
type ColorField = (typeof COLOR_FIELDS)[number];

const CSS_MAP: Record<ColorField, string> = {
  accent: '--accent',
  bg: '--bg',
  surface: '--surface',
  text: '--text',
  danger: '--danger',
  green: '--green',
};

// Defaults are sourced live from /shared/tokens.css (:root custom properties).
// Captured on module load, BEFORE any /api/branding overrides are applied via
// setProperty, so getComputedStyle returns the stylesheet defaults.
const COLOR_DEFAULTS: Record<ColorField, string> = (() => {
  const cs = getComputedStyle(document.documentElement);
  const out = {} as Record<ColorField, string>;
  for (const f of COLOR_FIELDS) out[f] = cs.getPropertyValue(`--${f}`).trim();
  return out;
})();

function setColorVar(field: ColorField, value: string | null): void {
  if (value) document.documentElement.style.setProperty(CSS_MAP[field], value);
  else document.documentElement.style.removeProperty(CSS_MAP[field]);
}

function getInput(id: string): HTMLInputElement {
  return document.getElementById(id) as HTMLInputElement;
}

export async function loadBranding(): Promise<void> {
  const res = await fetch('/api/branding');
  if (!res.ok) return;
  const data: BrandingResponse = await res.json();

  const logoPreview = document.getElementById('logo-preview') as HTMLImageElement;
  const logoEmpty = document.getElementById('logo-empty');
  if (logoPreview) logoPreview.style.display = data.hasLogo ? '' : 'none';
  if (logoEmpty) logoEmpty.style.display = data.hasLogo ? 'none' : '';
  if (data.hasLogo && logoPreview) logoPreview.src = '/api/branding/logo?' + Date.now();

  const brandImg = document.getElementById('brand-logo') as HTMLImageElement | null;
  if (brandImg) {
    if (data.hasLogo) {
      brandImg.src = '/api/branding/logo?' + Date.now();
      brandImg.style.display = '';
    } else {
      brandImg.style.display = 'none';
    }
  }

  // Brand name: visible in the header wordmark (when no logo) and the tab title,
  // so a Site Name change takes effect immediately.
  const siteName = data.siteName || 'Farbstrom';
  document.title = `${siteName} — Admin`;
  const brandName = document.getElementById('brand-name');
  if (brandName) {
    brandName.textContent = siteName;
    brandName.classList.toggle('u-hidden', data.hasLogo);
  }

  const bgPreview = document.getElementById('bg-preview') as HTMLImageElement | null;
  const bgEmpty = document.getElementById('bg-empty');
  if (bgPreview) bgPreview.style.display = data.hasBg ? '' : 'none';
  if (bgEmpty) bgEmpty.style.display = data.hasBg ? 'none' : '';
  if (data.hasBg && bgPreview) bgPreview.src = '/api/branding/bg?' + Date.now();

  const faviconPreview = document.getElementById('favicon-preview') as HTMLImageElement | null;
  const faviconEmpty = document.getElementById('favicon-empty');
  if (faviconPreview) faviconPreview.style.display = data.hasFavicon ? '' : 'none';
  if (faviconEmpty) faviconEmpty.style.display = data.hasFavicon ? 'none' : '';
  if (data.hasFavicon && faviconPreview)
    faviconPreview.src = '/api/branding/favicon?' + Date.now();

  const siteNameInput = document.getElementById('site-name-input') as HTMLInputElement | null;
  if (siteNameInput) siteNameInput.value = data.siteName ?? '';

  // Seed unconditionally. The markup carries no default colors — tokens.css is
  // the only copy of the palette — so if `colors` is absent the native color
  // inputs would otherwise sit at the UA default (#000000) rather than the
  // real defaults captured in COLOR_DEFAULTS.
  for (const f of COLOR_FIELDS) {
    const override = data.colors?.[`color_${f}`];
    const val = override || COLOR_DEFAULTS[f];
    getInput(`color-${f}`).value = val;
    getInput(`color-${f}-hex`).value = val;
    if (override) setColorVar(f, val);
  }
}

type BrandAsset = 'logo' | 'bg' | 'favicon';

const ASSET_LABEL: Record<BrandAsset, string> = {
  logo: 'Logo',
  bg: 'Background',
  favicon: 'Favicon',
};

// Mirror the backend allowlist so a wrong type fails with an inline message
// instead of a 400. SVG is rejected everywhere (it can carry inline scripts).
function assetTypeOk(asset: BrandAsset, type: string): boolean {
  switch (asset) {
    case 'logo':
      return type === 'image/png';
    case 'bg':
      return type === 'image/jpeg' || type === 'image/jpg';
    case 'favicon':
      return (
        type === 'image/png' || type === 'image/x-icon' || type === 'image/vnd.microsoft.icon'
      );
  }
}

const ASSET_TYPE_ERR: Record<BrandAsset, string> = {
  logo: 'Logo must be a PNG',
  bg: 'Background must be a JPEG',
  favicon: 'Favicon must be a PNG or ICO',
};

async function uploadBrandingAsset(asset: BrandAsset): Promise<void> {
  const input = getInput(`${asset}-file-input`);
  const file = input.files?.[0];
  if (!file) return;
  if (!assetTypeOk(asset, file.type)) {
    input.value = '';
    toast(ASSET_TYPE_ERR[asset]);
    return;
  }
  const fd = new FormData();
  fd.append('file', file);
  const res = await fetch(`/api/admin/branding/${asset}`, {
    method: 'POST',
    headers: { Authorization: `Bearer ${getToken()}` },
    body: fd,
  });
  input.value = '';
  if (res.ok) {
    toast(`${ASSET_LABEL[asset]} updated`);
    void loadBranding();
  } else {
    toast('Upload failed');
  }
}

async function removeBrandingAsset(asset: BrandAsset): Promise<void> {
  if (
    !(await confirmModal({
      title: `Remove ${ASSET_LABEL[asset]}`,
      message: `The custom ${ASSET_LABEL[asset].toLowerCase()} will be removed and the default restored.`,
      confirmLabel: 'Remove',
      danger: true,
    }))
  )
    return;
  const res = await apiFetch(`/api/admin/branding/${asset}`, { method: 'DELETE' });
  if (res && res.ok) {
    toast('Removed');
    void loadBranding();
  } else {
    toast('Remove failed');
  }
}

async function saveSiteName(): Promise<void> {
  const siteName = getInput('site-name-input').value.trim();
  const res = await apiFetch('/api/admin/branding/site-name', {
    method: 'POST',
    body: JSON.stringify({ siteName }),
  });
  if (res && res.ok) {
    toast('Site name saved');
    void loadBranding();
  } else {
    toast('Save failed');
  }
}

async function saveColors(): Promise<void> {
  const body: Record<string, string> = {};
  for (const f of COLOR_FIELDS) {
    body[`color_${f}`] = getInput(`color-${f}-hex`).value || '';
  }
  const res = await apiFetch('/api/admin/branding/colors', {
    method: 'POST',
    body: JSON.stringify(body),
  });
  if (res && res.ok) {
    for (const f of COLOR_FIELDS) {
      const val = body[`color_${f}`];
      setColorVar(f, val ? val : null);
    }
    toast('Colors saved');
  } else {
    toast('Save failed');
  }
}

async function resetColors(): Promise<void> {
  const body: Record<string, string> = {};
  for (const f of COLOR_FIELDS) {
    body[`color_${f}`] = '';
    getInput(`color-${f}`).value = COLOR_DEFAULTS[f];
    getInput(`color-${f}-hex`).value = COLOR_DEFAULTS[f];
  }
  const res = await apiFetch('/api/admin/branding/colors', {
    method: 'POST',
    body: JSON.stringify(body),
  });
  if (res && res.ok) {
    for (const f of COLOR_FIELDS) setColorVar(f, null);
    toast('Colors reset to defaults');
  } else {
    toast('Reset failed');
  }
}

export function initBranding(): void {
  // Live preview: color picker ↔ hex input, in-memory only until Save.
  for (const f of COLOR_FIELDS) {
    getInput(`color-${f}`).addEventListener('input', (e) => {
      const v = (e.target as HTMLInputElement).value;
      getInput(`color-${f}-hex`).value = v;
      setColorVar(f, v);
    });
    getInput(`color-${f}-hex`).addEventListener('input', (e) => {
      const v = (e.target as HTMLInputElement).value;
      if (/^#[0-9a-fA-F]{6}$/.test(v)) {
        getInput(`color-${f}`).value = v;
        setColorVar(f, v);
      }
    });
  }

  document.getElementById('colors-save-btn')?.addEventListener('click', saveColors);
  document.getElementById('colors-reset-btn')?.addEventListener('click', resetColors);

  document
    .getElementById('logo-upload-btn')
    ?.addEventListener('click', () => getInput('logo-file-input').click());
  document
    .getElementById('logo-file-input')
    ?.addEventListener('change', () => uploadBrandingAsset('logo'));
  document
    .getElementById('logo-remove-btn')
    ?.addEventListener('click', () => removeBrandingAsset('logo'));
  document
    .getElementById('bg-upload-btn')
    ?.addEventListener('click', () => getInput('bg-file-input').click());
  document
    .getElementById('bg-file-input')
    ?.addEventListener('change', () => uploadBrandingAsset('bg'));
  document
    .getElementById('bg-remove-btn')
    ?.addEventListener('click', () => removeBrandingAsset('bg'));

  document
    .getElementById('favicon-upload-btn')
    ?.addEventListener('click', () => getInput('favicon-file-input').click());
  document
    .getElementById('favicon-file-input')
    ?.addEventListener('change', () => uploadBrandingAsset('favicon'));
  document
    .getElementById('favicon-remove-btn')
    ?.addEventListener('click', () => removeBrandingAsset('favicon'));

  document.getElementById('site-name-save-btn')?.addEventListener('click', saveSiteName);
}

// Apply saved branding colors before any UI renders. Called once on load.
export function applyBrandingColorsOnce(): void {
  fetch('/api/branding')
    .then((r) => (r.ok ? r.json() : null))
    .then((data: BrandingResponse | null) => {
      if (!data?.colors) return;
      for (const f of COLOR_FIELDS) {
        const v = data.colors[`color_${f}`];
        if (v) document.documentElement.style.setProperty(CSS_MAP[f], v);
      }
    })
    .catch(() => {});
}

// On the login screen, show the uploaded custom logo, or the brand-name
// wordmark when none. Both start hidden in the HTML so neither flashes before
// /api/branding resolves; exactly one is revealed here. Also points the favicon
// at a custom brand icon when one is set.
export function applyLoginLogoOnce(): void {
  const logo = document.getElementById('login-logo') as HTMLImageElement | null;
  const title = document.getElementById('login-title');
  const showTitle = () => title?.classList.remove('u-hidden');
  fetch('/api/branding')
    .then((r) => (r.ok ? r.json() : null))
    .then((data: BrandingResponse | null) => {
      if (data?.siteName && title) title.textContent = data.siteName;
      if (data?.hasFavicon) {
        document
          .querySelectorAll('link[rel~="icon"], link[rel="apple-touch-icon"]')
          .forEach((l) => l.remove());
        const link = document.createElement('link');
        link.rel = 'icon';
        link.href = '/api/branding/favicon?' + Date.now();
        document.head.appendChild(link);
      }
      if (data?.hasLogo && logo) {
        logo.src = '/api/branding/logo?' + Date.now();
        logo.classList.remove('u-hidden');
      } else {
        showTitle();
      }
    })
    .catch(showTitle);
}
