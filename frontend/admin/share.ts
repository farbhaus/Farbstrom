// Room invite sharing (issue #182). Composes a plain-text invite for a room's
// guest link and hands it to the admin's own mail client as a mailto: URL.
//
// Plain text is not a shortcut: mailto: bodies are text/plain by RFC 6068, so
// there is no way to carry HTML through one. A styled HTML invite would mean
// the backend sending mail itself over SMTP, which this deliberately does not
// do. Only the guest link is shareable here — the host link is a capability in
// a URL fragment and stays copy-only.

import { closeModal, openModal, wireModalClose } from '../shared/components.js';
import { parseDbDate, toast } from '../shared/utils.js';
import type { BrandingResponse, Room } from './types.js';

let shareRoom: Room | null = null;
let shareUrl = '';

// Resolved once from the public branding endpoint, then reused.
//
// getBrandName() is not usable here: it reads <meta name="brand-name">, which
// routes::pages injects only into the landing and viewer documents, so on the
// admin page it always reports the default. The #brand-name span is no better
// — it holds the literal default until the Branding tab is opened. Either one
// would put the wrong name in every invite a rebranded install sends.
let brandName = '';

async function resolveBrand(): Promise<string> {
  if (brandName) return brandName;
  try {
    const res = await fetch('/api/branding');
    if (res.ok) {
      const data = (await res.json()) as BrandingResponse;
      brandName = data.siteName?.trim() || 'Farbstrom';
    }
  } catch {
    // Offline or blocked — fall through to the default, and retry next open.
  }
  return brandName || 'Farbstrom';
}

// The recipient is usually not in the sender's timezone, so the invite spells
// it out. fmtDateTime() is left alone: it renders bare local time, which is
// right everywhere else in admin and wrong the moment it leaves this machine.
function when(s: string | null | undefined): string | null {
  const d = parseDbDate(s);
  if (!d) return null;
  return d.toLocaleString('en-GB', {
    day: 'numeric',
    month: 'short',
    year: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
    timeZoneName: 'short',
  });
}

export interface InviteOpts {
  brand: string;
  url: string;
  /** Empty means the invite carries no password. */
  password: string;
}

export function buildInvite(r: Room, o: InviteOpts): { subject: string; body: string } {
  const starts = when(r.starts_at);

  // The start time is appended with a dash, not "on {time}" — the sentence has
  // already spent its "on" on the site name.
  const subject = starts
    ? `You've been invited to a remote session on ${o.brand} — ${starts}`
    : `You've been invited to a remote session on ${o.brand}`;

  // Labels are deliberately not padded into columns — mail clients render in a
  // proportional font, so the alignment would not survive the trip.
  const lines: string[] = [
    'Hello there,',
    '',
    `You're invited to join ${r.name} on ${o.brand}`,
    '',
    `Room: ${r.name}`,
  ];
  // Between Room and Link: both describe the room, and the link stays the last
  // thing read before the sign-off.
  if (starts) lines.push(`Starts: ${starts}`);
  lines.push(`Link: ${o.url}`);
  if (o.password) lines.push(`Password: ${o.password}`);
  lines.push(
    '',
    'Please disable True Tone and Night Shift, and dim your surroundings before the session.',
    '',
    'Set. Stream. Go!',
  );

  // CRLF is what RFC 6068 asks for in a mailto body.
  return { subject, body: lines.join('\r\n') };
}

// Addresses are percent-encoded one at a time so the commas separating them
// stay literal, which is the shape RFC 6068 specifies. encodeURIComponent turns
// the body's CRLFs into %0D%0A on its own.
//
// mailto: URLs have a practical ceiling near 2000 characters. This body runs to
// roughly 450 and every input to it is bounded (one room name, one URL, one
// password), so there is nothing here worth guarding against.
export function buildMailto(to: string, subject: string, body: string): string {
  const addrs = splitAddresses(to).map(encodeURIComponent).join(',');
  return `mailto:${addrs}?subject=${encodeURIComponent(subject)}&body=${encodeURIComponent(body)}`;
}

function splitAddresses(to: string): string[] {
  return to
    .split(/[,;]/)
    .map((s) => s.trim())
    .filter(Boolean);
}

function el<T extends HTMLElement>(id: string): T | null {
  return document.getElementById(id) as T | null;
}

export function openShareModal(r: Room, url: string): void {
  shareRoom = r;
  shareUrl = url;

  const to = el<HTMLInputElement>('share-to');
  const box = el<HTMLInputElement>('share-include-password');
  const pw = el<HTMLInputElement>('share-password');
  if (to) to.value = '';
  if (box) box.checked = false;
  if (pw) pw.value = '';

  const link = el('share-link');
  if (link) link.textContent = url;

  // A room with no password has nothing to offer, so the whole control goes.
  // Inline display (not .u-hidden) matches #clear-password-row, and sidesteps
  // .form-row.checkbox outranking a single utility class.
  const pwRow = el('share-password-row');
  if (pwRow) pwRow.style.display = r.password_hash ? '' : 'none';
  const pwField = el('share-password-field');
  if (pwField) pwField.style.display = 'none';

  openModal('share-modal');
  to?.focus();
}

async function sendShare(): Promise<void> {
  const r = shareRoom;
  if (!r) return;

  const to = el<HTMLInputElement>('share-to')?.value.trim() || '';
  const bad = splitAddresses(to).find((a) => !/^[^\s@]+@[^\s@]+$/.test(a));
  if (bad) {
    toast(`Not an email address: ${bad}`);
    return;
  }

  const wantsPassword = el<HTMLInputElement>('share-include-password')?.checked === true;
  const password = wantsPassword ? el<HTMLInputElement>('share-password')?.value.trim() || '' : '';
  if (wantsPassword && !password) {
    toast('Type the room password, or untick the box');
    return;
  }

  const brand = await resolveBrand();
  const { subject, body } = buildInvite(r, { brand, url: shareUrl, password });

  closeModal('share-modal');
  // With no mail handler registered, assigning a mailto: is a silent no-op —
  // this toast is the only thing telling the admin what was attempted.
  toast('Opening your mail app');
  window.location.href = buildMailto(to, subject, body);
}

export function initShare(): void {
  wireModalClose('share-modal', ['share-modal-close', 'share-modal-cancel']);
  el('share-modal-send')?.addEventListener('click', () => void sendShare());

  el('share-include-password')?.addEventListener('change', (e) => {
    const on = (e.target as HTMLInputElement).checked;
    const field = el('share-password-field');
    if (field) field.style.display = on ? '' : 'none';
    if (on) el<HTMLInputElement>('share-password')?.focus();
  });

  el('share-to')?.addEventListener('keydown', (e) => {
    if ((e as KeyboardEvent).key === 'Enter') void sendShare();
  });
}
