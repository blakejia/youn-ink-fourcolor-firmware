import test from 'node:test';
import assert from 'node:assert/strict';
import { windowActivity } from './activity.js';

test('windowActivity sums monotonic counter deltas', () => {
  const points = [
    { awake_ms: 1000, radio_ms: 400, epd_refreshes: 2, epd_busy_ms: 5000 },
    { awake_ms: 4000, radio_ms: 900, epd_refreshes: 5, epd_busy_ms: 12000 },
  ];
  assert.deepEqual(windowActivity(points), {
    awake_ms: 3000,
    radio_ms: 500,
    epd_refreshes: 3,
    epd_busy_ms: 7000,
  });
});

test('windowActivity treats a counter reset as activity since reboot', () => {
  const points = [
    { awake_ms: 9000, radio_ms: 2000, epd_refreshes: 9, epd_busy_ms: 30000 },
    { awake_ms: 500, radio_ms: 100, epd_refreshes: 1, epd_busy_ms: 2000 },
  ];
  assert.deepEqual(windowActivity(points), {
    awake_ms: 500,
    radio_ms: 100,
    epd_refreshes: 1,
    epd_busy_ms: 2000,
  });
});

test('windowActivity needs at least two samples and tolerates old rows', () => {
  assert.deepEqual(windowActivity([]), {
    awake_ms: 0,
    radio_ms: 0,
    epd_refreshes: 0,
    epd_busy_ms: 0,
  });
  assert.deepEqual(windowActivity([{ awake_ms: 42 }]), {
    awake_ms: 0,
    radio_ms: 0,
    epd_refreshes: 0,
    epd_busy_ms: 0,
  });
  assert.deepEqual(windowActivity([
    { awake_ms: 10, radio_ms: 2, epd_refreshes: 1, epd_busy_ms: 3 },
    { awake_ms: 20 },
  ]), {
    awake_ms: 10,
    radio_ms: 0,
    epd_refreshes: 0,
    epd_busy_ms: 0,
  });
});
