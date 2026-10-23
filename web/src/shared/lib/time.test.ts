import { describe, expect, it } from 'vitest';

import { relativeTime } from './time';

describe('relativeTime', () => {
  const now = 1_800_000_000;
  it('picks the largest unit', () => {
    const format = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });
    expect(relativeTime(now - 30, now)).toBe(format.format(-30, 'second'));
    expect(relativeTime(now - 5 * 60, now)).toBe(format.format(-5, 'minute'));
    expect(relativeTime(now + 24 * 3600, now)).toBe(format.format(1, 'day'));
  });
});
