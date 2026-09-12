// Locale-aware formatting. The API hands back raw epoch seconds (devices) and
// raw byte counts (OTA); neither belongs on screen unformatted. Intl keeps the
// output correct per browser locale instead of hardcoding a date pattern.

const dateTime = new Intl.DateTimeFormat('zh-CN', { dateStyle: 'medium', timeStyle: 'short' });
const relative = new Intl.RelativeTimeFormat('zh-CN', { numeric: 'auto' });

const UNITS = [
  ['year', 31536000],
  ['month', 2592000],
  ['week', 604800],
  ['day', 86400],
  ['hour', 3600],
  ['minute', 60],
];

/** Absolute local time, e.g. 2026年9月13日 11:35. Use as the `title` of a relative time. */
export function formatTime(epochSeconds) {
  if (!epochSeconds) return '—';
  return dateTime.format(new Date(epochSeconds * 1000));
}

/** Relative time, e.g. 3 分钟前. Falls back to the absolute time on clock skew. */
export function timeAgo(epochSeconds, nowMs = Date.now()) {
  if (!epochSeconds) return '—';
  const diff = nowMs / 1000 - epochSeconds;
  if (diff < 0) return formatTime(epochSeconds);
  if (diff < 60) return '刚刚';
  for (const [unit, secs] of UNITS) {
    if (diff >= secs) return relative.format(-Math.round(diff / secs), unit);
  }
  return '刚刚';
}

/** Short countdown for a one-shot token, e.g. 4 分 57 秒. */
export function formatCountdown(seconds) {
  if (!Number.isFinite(seconds) || seconds < 0) return '—';
  const minutes = Math.floor(seconds / 60);
  const rest = Math.round(seconds % 60);
  return minutes === 0 ? `${rest}\u00A0秒` : `${minutes}\u00A0分 ${rest}\u00A0秒`;
}

/** Byte count with a non-breaking space before the unit, e.g. 2.8 MB. */
export function formatBytes(bytes) {
  if (!Number.isFinite(bytes)) return '—';
  const units = ['B', 'KB', 'MB', 'GB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(unit === 0 ? 0 : 1)}\u00A0${units[unit]}`;
}
