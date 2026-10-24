import { describe, expect, it } from 'vitest';

import { emptyRule, sameRules, toRuleConfig, toRuleValues } from './rules';

describe('policy rules', () => {
  it('sends only the fields that are set', () => {
    expect(toRuleConfig({ ...emptyRule(), hosts: ['a.example'] })).toEqual({
      hosts: ['a.example'],
      action: 'allow',
    });
    expect(
      toRuleConfig({
        ...emptyRule(),
        name: ' no-delete ',
        hosts: ['api.github.com'],
        methods: ['delete'],
        paths: ['/repos/**'],
        action: 'deny',
      }),
    ).toEqual({
      name: 'no-delete',
      hosts: ['api.github.com'],
      methods: ['DELETE'],
      paths: ['/repos/**'],
      action: 'deny',
    });
  });

  it('round-trips rules from the API', () => {
    const rule = {
      name: 'ask-7',
      hosts: ['docs.example.com'],
      methods: ['GET'],
      action: 'allow' as const,
    };
    expect(toRuleConfig(toRuleValues(rule))).toEqual(rule);
  });

  it('treats missing and empty lists as the same', () => {
    expect(
      sameRules(
        [{ hosts: ['a'], action: 'allow' }],
        [{ hosts: ['a'], methods: [], paths: [], name: null, action: 'allow' }],
      ),
    ).toBe(true);
    expect(sameRules([{ hosts: ['a'], action: 'allow' }], [{ hosts: ['a'], action: 'deny' }])).toBe(
      false,
    );
  });
});
