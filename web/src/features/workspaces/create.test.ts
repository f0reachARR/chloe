import { describe, expect, it } from 'vitest';

import { emptyCreateValues, toCreateBody, validateName } from './create';

describe('toCreateBody', () => {
  it('joins the owner prefix and sends only what was set', () => {
    const values = emptyCreateValues('team:acme/dev');
    expect(toCreateBody('acme/dev/', { ...values, name: 'Web' })).toEqual({ name: 'acme/dev/web' });
    expect(
      toCreateBody('', {
        ...values,
        name: 'api',
        root: 'persistent',
        vcpus: 8,
        memoryGib: 1.5,
        workspaceDiskGib: 50,
        dockerDiskGib: '',
        idle: 'after',
        idleAfter: '2h',
      }),
    ).toEqual({
      name: 'api',
      root: 'persistent',
      vcpus: 8,
      memory_mib: 1536,
      workspace_disk_gib: 50,
      idle_timeout: '2h',
    });
    expect(toCreateBody('acme/', { ...values, name: 'x', idle: 'never' })).toEqual({
      name: 'acme/x',
      idle_timeout: 'never',
    });
  });
});

describe('validateName', () => {
  it('follows the gateway rules', () => {
    for (const ok of ['web', 'my_ws', 'a-b', '_x', 'A1']) expect(validateName(ok)).toBeNull();
    for (const bad of ['', '-x', 'a/b', 'a b', 'x'.repeat(65)]) {
      expect(validateName(bad)).not.toBeNull();
    }
  });
});
