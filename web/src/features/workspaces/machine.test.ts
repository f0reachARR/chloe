import { describe, expect, it } from 'vitest';

import { workspace } from '@/test/fixtures';

import { formatIdle, needsRestart, toPatch, toValues } from './machine';

describe('machine settings', () => {
  it('writes idle times the way chloe ws idle reads them', () => {
    expect(formatIdle(1800)).toBe('30m');
    expect(formatIdle(7200)).toBe('2h');
    expect(formatIdle(86400)).toBe('1d');
    expect(formatIdle(90)).toBe('90s');
  });

  it('reads a workspace that never stops', () => {
    expect(toValues(workspace({ idle_timeout_secs: null })).idle).toBe('never');
  });

  it('sends only what changed', () => {
    const initial = toValues(workspace());
    expect(toPatch(initial, initial)).toEqual({});
    expect(toPatch(initial, { ...initial, vcpus: 8, memoryGib: 16 })).toEqual({
      vcpus: 8,
      memory_mib: 16384,
    });
    expect(toPatch(initial, { ...initial, idleAfter: '2h' })).toEqual({ idle_timeout: '2h' });
    expect(toPatch(initial, { ...initial, idle: 'default' })).toEqual({ idle_timeout: 'default' });
    // `never` のあいだに書き換えた時間は送らない。
    expect(toPatch(initial, { ...initial, idle: 'never', idleAfter: '2h' })).toEqual({
      idle_timeout: 'never',
    });
  });

  it('knows what applies from the next start', () => {
    expect(needsRestart({ idle_timeout: '2h' })).toBe(false);
    expect(needsRestart({ root: 'persistent' })).toBe(true);
    expect(needsRestart({ docker_disk_gib: 80 })).toBe(true);
  });
});
