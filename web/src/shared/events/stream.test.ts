import { describe, expect, it, vi } from 'vitest';

import { type LiveState, RESYNC, connectEvents } from './stream';

// EventSource の代わり。テストから出来事を流す。
class FakeSource extends EventTarget {
  readyState = 0;
  closed = false;
  close() {
    this.closed = true;
  }
  emit(name: string, data?: string) {
    this.dispatchEvent(
      name === 'open' || name === 'error' ? new Event(name) : new MessageEvent(name, { data }),
    );
  }
}

function setup() {
  const sources: FakeSource[] = [];
  const states: LiveState[] = [];
  const ask = vi.fn<(data: unknown) => void>();
  const resync = vi.fn<(data: unknown) => void>();
  const stop = connectEvents({
    url: '/api/v1/events',
    listeners: { ask: [ask], [RESYNC]: [resync] },
    onState: (state) => states.push(state),
    retryMs: 10,
    createSource: () => {
      const source = new FakeSource();
      sources.push(source);
      return source as unknown as EventSource;
    },
  });
  return { sources, states, ask, resync, stop };
}

describe('connectEvents', () => {
  it('dispatches named events and resyncs on open and lag', () => {
    const { sources, states, ask, resync, stop } = setup();
    const source = sources[0]!;
    source.emit('open');
    expect(states).toEqual(['connecting', 'live']);
    expect(resync).toHaveBeenCalledTimes(1);

    source.emit('ask', '{"type":"created","id":3,"workspace":"api"}');
    expect(ask).toHaveBeenCalledWith({ type: 'created', id: 3, workspace: 'api' });

    source.emit('lagged', '{}');
    expect(resync).toHaveBeenCalledTimes(2);
    stop();
    expect(source.closed).toBe(true);
  });

  it('reconnects after the server refuses the stream', async () => {
    vi.useFakeTimers();
    const { sources, states, stop } = setup();
    sources[0]!.readyState = 2; // CLOSED
    sources[0]!.emit('error');
    expect(states.at(-1)).toBe('offline');
    await vi.advanceTimersByTimeAsync(10);
    expect(sources).toHaveLength(2);
    stop();
    vi.useRealTimers();
  });
});
