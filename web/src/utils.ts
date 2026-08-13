const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });
const dtf = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeZone: 'UTC' });

export function timeAgo(iso: string, now: number = Date.now()): string {
  const ms = new Date(iso).getTime();
  if (isNaN(ms)) return '';
  const seconds = Math.floor((now - ms) / 1000);
  if (seconds < 60) return rtf.format(0, 'second');
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return rtf.format(-minutes, 'minute');
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return rtf.format(-hours, 'hour');
  const days = Math.floor(hours / 24);
  return rtf.format(-days, 'day');
}

/** Format a Steam date as a literal calendar day — noon anchor so no timezone shifts it. */
export function formatDate(iso: string): string {
  const d = new Date(`${iso.slice(0, 10)}T12:00:00Z`);
  return isNaN(d.getTime()) ? '' : dtf.format(d);
}

const pacificDateFmt = new Intl.DateTimeFormat('en-CA', {
  timeZone: 'America/Los_Angeles',
  year: 'numeric',
  month: '2-digit',
  day: '2-digit',
});

/** Check whether an ISO date string falls on "today" in US/Pacific (Steam's reporting TZ). */
export function isTodayPacific(iso: string): boolean {
  if (!iso) return false;
  const snapshotDate = iso.slice(0, 10); // "YYYY-MM-DD"
  const todayDate = pacificDateFmt.format(new Date()); // "YYYY-MM-DD" (en-CA uses this format)
  return snapshotDate === todayDate;
}

const pacificTimeFmt = new Intl.DateTimeFormat('en-GB', {
  timeZone: 'America/Los_Angeles',
  hour: '2-digit',
  minute: '2-digit',
});

/** Format a UTC instant as "14:05 PT". */
export function formatTimePacific(iso: string): string {
  const d = new Date(iso);
  return isNaN(d.getTime()) ? '' : `${pacificTimeFmt.format(d)} PT`;
}

const steamDayFmt = new Intl.DateTimeFormat(undefined, {
  timeZone: 'UTC',
  weekday: 'short',
  year: 'numeric',
  month: '2-digit',
  day: '2-digit',
});

/** Format a bare Steam date ("YYYY-MM-DD") as a literal calendar date.
 *  Anchored at noon UTC so no timezone renders the neighboring day. */
export function formatSteamDate(ymd: string): string {
  const d = new Date(`${ymd.slice(0, 10)}T12:00:00Z`);
  return isNaN(d.getTime()) ? ymd : steamDayFmt.format(d);
}

/** Local (browser TZ) YYYY-MM-DD. */
export function toLocalYMD(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
}

const monthFmt = new Intl.DateTimeFormat(undefined, {
  timeZone: 'UTC',
  month: 'short',
  year: 'numeric',
});

export function formatChartLabel(label: string, date: string, resolution: string): string {
  if (resolution === 'raw') return `${formatSteamDate(date)} · ${formatTimePacific(label)}`;
  if (resolution === 'daily') return formatSteamDate(label);
  if (resolution === 'weekly') {
    const [y, w] = label.split('-W');
    return w ? `Week ${Number(w)}, ${y}` : label;
  }
  if (resolution === 'monthly') {
    const d = new Date(`${label}-15T12:00:00Z`);
    return isNaN(d.getTime()) ? label : monthFmt.format(d);
  }
  return label;
}

/** Return how many whole minutes have elapsed since an ISO 8601 timestamp. */
export function minutesAgo(iso: string | null, now: number = Date.now()): number {
  if (!iso) return 0;
  const ms = new Date(iso).getTime();
  if (isNaN(ms)) return 0;
  return Math.max(0, Math.round((now - ms) / 60_000));
}

export function countryFlag(code: string): string {
  if (!code || !/^[a-zA-Z]{2}$/.test(code)) return "";
  return [...code.toUpperCase()].map(c => String.fromCodePoint(0x1F1E6 + c.charCodeAt(0) - 65)).join("");
}

export function formatNumber(n: number): string {
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + 'M';
  if (n >= 1_000) return (n / 1_000).toFixed(1) + 'K';
  return n.toString();
}
