// Shared auth state + the apiFetch wrapper every other admin module uses.
// Holds the JWT in localStorage and triggers a logout on 401.

const TOKEN_KEY = 'stream_token';

let token = localStorage.getItem(TOKEN_KEY) || '';
let onLogout: (() => void) | null = null;

export function getToken(): string {
  return token;
}

export function setToken(value: string): void {
  token = value;
  fileLink = null;
  localStorage.setItem(TOKEN_KEY, value);
}

export function clearToken(): void {
  token = '';
  fileLink = null;
  localStorage.removeItem(TOKEN_KEY);
}

export function setLogoutHandler(fn: () => void): void {
  onLogout = fn;
}

export async function apiFetch(path: string, opts: RequestInit = {}): Promise<Response | null> {
  const headers: Record<string, string> = {
    Authorization: `Bearer ${token}`,
    'Content-Type': 'application/json',
    ...((opts.headers as Record<string, string> | undefined) ?? {}),
  };
  const res = await fetch(path, { ...opts, headers });
  // 401 from an admin endpoint means one thing only: this session is no longer
  // valid — expired, or revoked by a password change / "sign out other
  // devices". Anything that is merely *refused* (a wrong password typed into a
  // confirmation form, say) is a 403 and is left for the caller to report;
  // returning 401 for those used to sign you out on a typo.
  if (res.status === 401) {
    if (onLogout) onLogout();
    return null;
  }
  return res;
}

/// Library previews and downloads are `<img>` / `<video>` / download-link URLs,
/// which cannot carry the session header — so they carry `?ft=`, a token that
/// opens library files and nothing else, lasts ten minutes, and dies with a
/// session revocation. Those URLs used to carry the session token itself.
/// Cached, and renewed while it still has a couple of minutes left; reset by
/// any new session token, since that can mean a new generation.
let fileLink: { token: string; expiresAt: number } | null = null;

export async function fileLinkToken(): Promise<string> {
  if (fileLink && fileLink.expiresAt - Date.now() > 120_000) return fileLink.token;
  const res = await apiFetch('/api/admin/files/link-token');
  if (!res || !res.ok) return '';
  const data = (await res.json().catch(() => null)) as { token?: string; ttlSeconds?: number } | null;
  if (!data?.token) return '';
  fileLink = { token: data.token, expiresAt: Date.now() + (data.ttlSeconds ?? 0) * 1000 };
  return data.token;
}

/// A short-lived token that lets this browser play any stream, passed as
/// `?apt=`. Browser playback is admission-gated — OME asks the backend before
/// every WebRTC or LL-HLS session — and this is the admin's way through. It is
/// checked when a session starts, so five minutes is plenty.
export async function fetchPreviewToken(): Promise<string | null> {
  const res = await apiFetch('/api/ome/playback-token');
  if (!res || !res.ok) return null;
  const data = (await res.json().catch(() => null)) as { token?: string } | null;
  return data?.token ?? null;
}

export interface LoginResult {
  ok: boolean;
  error?: string;
  totpRequired?: boolean;
}

/// The sign-in endpoints are rate limited per IP, and the limiter answers 429
/// in plain text — which the JSON read below turned into a bare "Sign in
/// failed", so a correct password typed after two typos read as wrong.
function rateLimited(res: Response): string | null {
  if (res.status !== 429) return null;
  const wait = parseInt(res.headers.get('retry-after') || '', 10);
  return Number.isFinite(wait) && wait > 0
    ? `Too many attempts — locked for ${wait} s`
    : 'Too many attempts';
}

/// `trust` asks the server for a 90-day session instead of the 7-day default —
/// the "trust this browser" checkbox. Only reasonable because sessions are now
/// revocable: "Sign out other devices" in Settings kills a trusted browser you
/// no longer have.
export async function login(
  password: string,
  totpCode?: string,
  trust = false,
): Promise<LoginResult> {
  let res: Response;
  try {
    res = await fetch('/api/auth/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ password, totp_code: totpCode, trust }),
    });
  } catch {
    return { ok: false, error: 'Cannot reach server — is the backend running?' };
  }
  const limited = rateLimited(res);
  if (limited) return { ok: false, error: limited };
  const data = await res.json().catch(() => ({}));
  if (!res.ok) return { ok: false, error: data.error || 'Sign in failed' };
  // Password OK but a 2FA code is still needed (HTTP 200, no token).
  if (data.totpRequired) return { ok: false, totpRequired: true };
  setToken(data.token);
  return { ok: true };
}

export interface AuthMethods {
  totpEnabled: boolean;
  passkeyEnabled: boolean;
}

export async function fetchAuthMethods(): Promise<AuthMethods> {
  try {
    const res = await fetch('/api/auth/methods');
    if (res.ok) return await res.json();
  } catch {
    /* fall through */
  }
  return { totpEnabled: false, passkeyEnabled: false };
}

// Run the WebAuthn assertion ceremony; sets the token on success.
export async function passkeyLogin(
  doAuthenticate: (options: unknown) => Promise<unknown>,
  trust = false,
): Promise<LoginResult> {
  let res: Response;
  try {
    res = await fetch('/api/auth/passkey/start', { method: 'POST' });
  } catch {
    return { ok: false, error: 'Cannot reach server' };
  }
  if (!res.ok) {
    const limited = rateLimited(res);
    if (limited) return { ok: false, error: limited };
    const e = await res.json().catch(() => ({}));
    return { ok: false, error: e.error || 'No passkeys available' };
  }
  const { id, options } = await res.json();
  let credential: unknown;
  try {
    credential = await doAuthenticate(options);
  } catch {
    return { ok: false, error: 'Passkey cancelled' };
  }
  const fin = await fetch('/api/auth/passkey/finish', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ id, credential, trust }),
  });
  const limitedFinish = rateLimited(fin);
  if (limitedFinish) return { ok: false, error: limitedFinish };
  const data = await fin.json().catch(() => ({}));
  if (!fin.ok) return { ok: false, error: data.error || 'Passkey rejected' };
  setToken(data.token);
  return { ok: true };
}
