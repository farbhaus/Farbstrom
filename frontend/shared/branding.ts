// Read-only branding loader: fetches /api/branding and applies logo,
// background image, and color overrides. Pages call this once at load.
//
// The admin SPA has its own branding.ts that handles editing — this is
// only for surfaces that want to display whatever is currently set.

const COLOR_MAP: Record<string, string> = {
  color_accent: '--accent',
  color_bg: '--bg',
  color_surface: '--surface',
  color_text: '--text',
  color_danger: '--danger',
  color_green: '--green',
};

interface BrandingPayload {
  hasLogo?: boolean;
  hasBg?: boolean;
  hasFavicon?: boolean;
  siteName?: string;
  colors?: Record<string, string>;
}

export interface ApplyBrandingOptions {
  logoEl?: HTMLImageElement | null;
  bgTarget?: HTMLElement | null;
  /** Set document.title to the configured brand name (landing page only —
   *  the viewer sets a per-room title itself via getBrandName()). */
  setTitle?: boolean;
}

// Configured brand name, defaulting to the shipped fallback until /api/branding
// resolves. Pages that build their own title (e.g. the viewer's per-room title)
// read this so the brand half reflects the deployment's branding.
let brandName = 'Farbstrom';
export function getBrandName(): string {
  return brandName;
}

/** Point the favicon at the uploaded brand icon, dropping the shipped defaults. */
function applyFavicon(): void {
  document.querySelectorAll('link[rel~="icon"], link[rel="apple-touch-icon"]').forEach((l) => l.remove());
  const link = document.createElement('link');
  link.rel = 'icon';
  link.href = '/api/branding/favicon?' + Date.now();
  document.head.appendChild(link);
}

export async function applyBranding(opts: ApplyBrandingOptions = {}): Promise<BrandingPayload | null> {
  try {
    const res = await fetch('/api/branding');
    if (!res.ok) return null;
    const data: BrandingPayload = await res.json();

    if (data.colors) {
      for (const [key, cssVar] of Object.entries(COLOR_MAP)) {
        const v = data.colors[key];
        if (v) document.documentElement.style.setProperty(cssVar, v);
      }
    }

    if (data.siteName) {
      brandName = data.siteName;
      // Wordmark fallbacks (shown when there's no logo) and, on request, the
      // document title reflect the configured brand name.
      document
        .querySelectorAll<HTMLElement>('.brand-wordmark, .brand-wordmark-sm')
        .forEach((el) => {
          el.textContent = data.siteName as string;
        });
      if (opts.setTitle) document.title = data.siteName;
    }

    if (data.hasFavicon) applyFavicon();

    if (data.hasLogo && opts.logoEl) {
      opts.logoEl.src = '/api/branding/logo';
      opts.logoEl.classList.remove('u-hidden');
    }

    if (data.hasBg) {
      const target = opts.bgTarget || document.body;
      target.style.backgroundImage = 'url(/api/branding/bg)';
      target.style.backgroundSize = 'cover';
      target.style.backgroundPosition = 'center';
      target.style.backgroundRepeat = 'no-repeat';
    }

    return data;
  } catch {
    return null;
  }
}
