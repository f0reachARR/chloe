import { describe, expect, it } from 'vitest';

import { agents } from './agents';

describe('agents', () => {
  it('groups activity marks by tool', () => {
    expect(agents(['claude-1b2c', 'claude-9f8e', 'codex-aaaa'])).toBe('claude ×2, codex');
    expect(agents(['claude-1b2c'])).toBe('claude');
  });
});
