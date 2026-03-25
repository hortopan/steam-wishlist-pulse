const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });
const dtf = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium' });

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

export function formatDate(iso: string): string {
  const d = new Date(iso);
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

/** Return how many whole minutes have elapsed since an ISO 8601 timestamp. */
export function minutesAgo(iso: string | null, now: number = Date.now()): number {
  if (!iso) return 0;
  const ms = new Date(iso).getTime();
  if (isNaN(ms)) return 0;
  return Math.max(0, Math.round((now - ms) / 60_000));
}

export function formatNumber(n: number): string {
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + 'M';
  if (n >= 1_000) return (n / 1_000).toFixed(1) + 'K';
  return n.toString();
}
