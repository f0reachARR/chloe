import { describe, expect, it } from 'vitest';

import { emptyPermission, sameLayer, toLayer, toLayerValues } from './layer';

describe('policy layers', () => {
  it('leaves out what is not set', () => {
    const values = toLayerValues({});
    expect(values).toMatchObject({ root: 'inherit', github: 'inherit', rules: [] });
    expect(toLayer(values, false)).toEqual({});
  });

  it('round-trips the root and the token scope', () => {
    const layer = {
      rules: [{ hosts: ['registry.example.com'], action: 'allow' as const }],
      root: 'persistent' as const,
      github: { repositories: ['web', 'api'], permissions: { contents: 'write' } },
    };
    expect(toLayer(toLayerValues(layer), false)).toEqual(layer);
    // リポジトリだけ、権限だけを絞れる。
    expect(toLayer(toLayerValues({ github: { permissions: { issues: 'read' } } }), false)).toEqual({
      github: { permissions: { issues: 'read' } },
    });
  });

  it('writes permissions from the rows', () => {
    const values = {
      ...toLayerValues({}),
      github: 'set' as const,
      permissions: 'listed' as const,
      permissionList: [
        { ...emptyPermission(), name: ' contents ', level: 'write' },
        { ...emptyPermission(), name: 'pull_requests', level: 'read' },
      ],
    };
    expect(toLayer(values, false)).toEqual({
      github: { permissions: { contents: 'write', pull_requests: 'read' } },
    });
  });

  it('never sends a token scope for personal settings', () => {
    const values = { ...toLayerValues({}), github: 'set' as const };
    expect(toLayer(values, true)).toEqual({});
    expect(toLayer(values, false)).toEqual({ github: {} });
  });

  it('compares layers by what they send', () => {
    expect(
      sameLayer(
        { github: { permissions: { a: 'read', b: 'write' } } },
        { rules: [], root: null, github: { permissions: { b: 'write', a: 'read' } } },
      ),
    ).toBe(true);
    expect(sameLayer({ root: 'persistent' }, {})).toBe(false);
  });
});
