// One-time, dismissible functional-storage notice. The app sets no tracking or
// third-party cookies — only functional browser storage (session JWT, per-room
// preferences) — so this is a disclosure, not a consent gate: nothing is
// blocked and dismissal is remembered in localStorage.

const ACK_KEY = 'privacy_notice_ack';

export function initPrivacyNotice(): void {
  try {
    if (localStorage.getItem(ACK_KEY)) return;
  } catch {
    // localStorage unavailable (private mode / blocked) — without it we can't
    // remember a dismissal, so don't nag; skip the notice entirely.
    return;
  }

  const bar = document.createElement('div');
  bar.className = 'privacy-notice';
  bar.setAttribute('role', 'note');

  const text = document.createElement('span');
  text.className = 'privacy-notice-text';
  text.textContent =
    'This site uses only functional browser storage (no tracking or third-party cookies) to keep you in your room and remember your preferences. ';
  const link = document.createElement('a');
  link.href = '/privacy';
  link.textContent = 'Learn more';
  text.appendChild(link);
  text.appendChild(document.createTextNode('.'));

  const btn = document.createElement('button');
  btn.className = 'btn btn-sm btn-primary privacy-notice-btn';
  btn.textContent = 'Got it';
  btn.addEventListener('click', () => {
    try {
      localStorage.setItem(ACK_KEY, '1');
    } catch {
      /* ignore */
    }
    bar.remove();
  });

  bar.appendChild(text);
  bar.appendChild(btn);
  document.body.appendChild(bar);
}
