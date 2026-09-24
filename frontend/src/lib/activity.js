// Relative activity estimator for the battery-detail modal.
//
// The server stores cumulative RTC counters at each battery-history sample.
// For a selected time window we sum adjacent deltas so the number describes
// activity inside that window, not since boot. A counter that decreases means
// the device rebooted (cold boot/power loss zeroes RTC memory); the activity
// after that reset is the new value, so we add it instead of a negative delta.
//
// No current sensor exists on NOTE4C, so these are deliberately NOT converted
// to mAh or a battery percentage. E-ink holds a static image with essentially
// no panel power, so EPD time here means waveform-busy time only.

const COUNTERS = ['awake_ms', 'radio_ms', 'epd_refreshes', 'epd_busy_ms'];

const num = v => (Number.isFinite(v) ? v : 0);

export function windowActivity(points) {
  const out = { awake_ms: 0, radio_ms: 0, epd_refreshes: 0, epd_busy_ms: 0 };
  if (!Array.isArray(points) || points.length < 2) return out;
  const prev = {};
  for (const key of COUNTERS) prev[key] = num(points[0][key]);
  for (let i = 1; i < points.length; i++) {
    for (const key of COUNTERS) {
      const cur = num(points[i][key]);
      out[key] += cur >= prev[key] ? cur - prev[key] : cur;
      prev[key] = cur;
    }
  }
  return out;
}
