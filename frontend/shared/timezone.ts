// Timezone helpers (gh #263). Room start/expiry times are stored as UTC
// instants; these name a zone, render an instant in it, and convert the
// admin's datetime-local wall-clock input to and from it. Zones are IANA names
// ("Europe/Lisbon") and everything goes through Intl, so there is no tz data
// to ship.

/** The zone this browser runs in. */
export function browserZone(): string {
  return new Intl.DateTimeFormat().resolvedOptions().timeZone;
}

/** Whether this browser knows the zone. Intl throws a RangeError otherwise. */
export function isKnownZone(zone: string | null | undefined): zone is string {
  if (!zone) return false;
  try {
    new Intl.DateTimeFormat('en-US', { timeZone: zone });
    return true;
  } catch {
    return false;
  }
}

/**
 * Every zone Intl lists, plus UTC, which some engines omit. The fixed-offset
 * Etc/ zones are left out — their names carry POSIX's inverted sign
 * (Etc/GMT+5 is GMT-5), which no one should have to pick through.
 */
function zoneList(): string[] {
  const zones = Intl.supportedValuesOf('timeZone').filter((z) => !z.startsWith('Etc/'));
  return zones.includes('UTC') ? zones : ['UTC', ...zones];
}

// The city each clock is listed under. It is the name invites and the waiting
// screen print, so it is chosen rather than left to the alphabet — Lisbon,
// never London, for Western European time. A clock with no city here takes
// the first zone Intl lists on it, by name.
const PICKER_CITIES = [
  'Pacific/Pago_Pago',
  'Pacific/Honolulu',
  'America/Adak',
  'Pacific/Marquesas',
  'America/Anchorage',
  'America/Los_Angeles',
  'America/Phoenix',
  'America/Denver',
  'America/Mexico_City',
  'America/Chicago',
  'America/Bogota',
  'America/New_York',
  'America/Caracas',
  'America/Halifax',
  'America/Santiago',
  'America/St_Johns',
  'America/Sao_Paulo',
  'America/Noronha',
  'America/Nuuk',
  'Atlantic/Cape_Verde',
  'Atlantic/Azores',
  'UTC',
  'Europe/Lisbon',
  'Africa/Lagos',
  'Europe/Paris',
  'Africa/Johannesburg',
  'Europe/Athens',
  'Europe/Istanbul',
  'Asia/Tehran',
  'Asia/Dubai',
  'Asia/Kabul',
  'Asia/Karachi',
  'Asia/Kolkata',
  'Asia/Kathmandu',
  'Asia/Dhaka',
  'Asia/Yangon',
  'Asia/Bangkok',
  'Asia/Singapore',
  'Asia/Tokyo',
  'Australia/Darwin',
  'Australia/Adelaide',
  'Australia/Brisbane',
  'Australia/Sydney',
  'Pacific/Noumea',
  'Pacific/Fiji',
  'Pacific/Auckland',
  'Pacific/Chatham',
  'Pacific/Tongatapu',
  'Pacific/Kiritimati',
];

/**
 * Zones for a picker: one per clock, sorted by its offset at `now`. Zones on
 * the same offset in January and in July keep the same time all year as far
 * as scheduling a session goes, so four hundred cities collapse to the few
 * dozen clocks there actually are.
 */
export function pickerZones(now: Date = new Date()): string[] {
  const y = now.getUTCFullYear();
  const jan = new Date(Date.UTC(y, 0, 15, 12));
  const jul = new Date(Date.UTC(y, 6, 15, 12));
  const clocks = new Map<string, { zone: string; year: number }>();
  // The chosen cities go first and are tried by name rather than looked up in
  // the list: engines accept a zone's current name even where they list only
  // its legacy alias (Chrome lists Asia/Calcutta, not Asia/Kolkata).
  for (const zone of [...PICKER_CITIES.filter(isKnownZone), ...zoneList()]) {
    const a = offsetMs(jan, zone);
    const b = offsetMs(jul, zone);
    const key = `${a}/${b}`;
    if (!clocks.has(key)) clocks.set(key, { zone, year: a + b });
  }
  return [...clocks.values()]
    .map((c) => ({ ...c, now: offsetMs(now, c.zone) }))
    .sort((p, q) => p.now - q.now || p.year - q.year)
    .map((c) => c.zone);
}

/** "GMT+1", "GMT-4", "GMT+5:30" — the zone's offset at that instant. */
export function offsetLabel(zone: string, at: Date): string {
  const part = new Intl.DateTimeFormat('en-US', {
    timeZone: zone,
    timeZoneName: 'shortOffset',
  })
    .formatToParts(at)
    .find((p) => p.type === 'timeZoneName');
  return part?.value ?? '';
}

// The place a zone is named after: the IANA name's last segment. UTC and the
// Etc/ zones have none.
function place(zone: string): string | null {
  if (!zone.includes('/') || zone.startsWith('Etc/')) return null;
  return (zone.split('/').pop() ?? zone).replace(/_/g, ' ');
}

/** "Lisbon time (GMT+1)"; a zone with no place is just its offset. */
export function zoneLabel(zone: string, at: Date): string {
  const p = place(zone);
  const off = offsetLabel(zone, at);
  return p ? `${p} time (${off})` : off;
}

interface Wall {
  y: number;
  mo: number;
  d: number;
  h: number;
  mi: number;
}

function wallParts(at: Date, zone: string): Wall {
  const parts = new Intl.DateTimeFormat('en-US', {
    timeZone: zone,
    year: 'numeric',
    month: 'numeric',
    day: 'numeric',
    hour: 'numeric',
    minute: 'numeric',
    hourCycle: 'h23',
  }).formatToParts(at);
  const n = (type: Intl.DateTimeFormatPartTypes): number =>
    Number(parts.find((p) => p.type === type)?.value ?? 0);
  return { y: n('year'), mo: n('month'), d: n('day'), h: n('hour'), mi: n('minute') };
}

// Wall clock minus UTC, in ms, at that instant (to the minute).
function offsetMs(at: Date, zone: string): number {
  const w = wallParts(at, zone);
  return Date.UTC(w.y, w.mo - 1, w.d, w.h, w.mi) - Math.floor(at.getTime() / 60000) * 60000;
}

const pad = (n: number): string => String(n).padStart(2, '0');

/** An instant as datetime-local's "YYYY-MM-DDTHH:MM", on the zone's clock. */
export function utcToWall(at: Date, zone: string): string {
  const w = wallParts(at, zone);
  return `${w.y}-${pad(w.mo)}-${pad(w.d)}T${pad(w.h)}:${pad(w.mi)}`;
}

const DAY_MS = 86_400_000;

/**
 * The instant a datetime-local value names on the zone's clock. Resolved the
 * way the browser resolves its own local time: a time that occurs twice (the
 * hour repeated in autumn) is the earlier one, and a time that never occurs
 * (the hour skipped in spring) moves forward by the gap.
 */
export function wallToUtc(v: string, zone: string): Date | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})/.exec(v);
  if (!m) return null;
  const wall = Date.UTC(+m[1]!, +m[2]! - 1, +m[3]!, +m[4]!, +m[5]!);
  // The offsets either side of any transition near this wall time; each one
  // that maps back to the same wall time is a reading of it.
  const before = offsetMs(new Date(wall - DAY_MS), zone);
  const after = offsetMs(new Date(wall + DAY_MS), zone);
  const fits = [wall - before, wall - after].filter(
    (t) => offsetMs(new Date(t), zone) === wall - t,
  );
  return new Date(fits.length ? Math.min(...fits) : wall - before);
}
