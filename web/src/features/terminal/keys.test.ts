import { describe, expect, it } from 'vitest';

import { withCtrl } from './keys';

describe('withCtrl', () => {
  it('turns a letter into its control character', () => {
    expect(withCtrl('c')).toBe('\x03');
    expect(withCtrl('C')).toBe('\x03');
    expect(withCtrl('[')).toBe('\x1b');
    expect(withCtrl('?')).toBe('\x7f');
  });

  it('leaves other input alone', () => {
    expect(withCtrl('1')).toBe('1');
    expect(withCtrl('ls')).toBe('ls');
  });
});
