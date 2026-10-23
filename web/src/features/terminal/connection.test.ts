import { describe, expect, it } from 'vitest';

import { type TerminalState, connectTerminal, terminalUrl } from './connection';

// WebSocket の代わり。テストからフレームを流し、送られたものを覚える。
class FakeSocket extends EventTarget {
  binaryType = 'blob';
  readyState: number = WebSocket.CONNECTING;
  sent: (string | Uint8Array)[] = [];
  closed = false;
  send(data: string | Uint8Array) {
    this.sent.push(data);
  }
  close() {
    this.closed = true;
  }
  open() {
    this.readyState = WebSocket.OPEN;
    this.dispatchEvent(new Event('open'));
  }
  receive(data: string | ArrayBuffer) {
    this.dispatchEvent(new MessageEvent('message', { data }));
  }
  drop() {
    this.readyState = WebSocket.CLOSED;
    this.dispatchEvent(new CloseEvent('close'));
  }
}

function setup() {
  const socket = new FakeSocket();
  const states: TerminalState[] = [];
  const output: string[] = [];
  const decoder = new TextDecoder();
  const connection = connectTerminal({
    url: 'ws://chloe.test/api/v1/workspace-terminal/api',
    onOutput: (data) => output.push(decoder.decode(data)),
    onState: (state) => states.push(state),
    createSocket: () => socket as unknown as WebSocket,
  });
  return { socket, states, output, connection };
}

describe('terminalUrl', () => {
  it('uses ws(s) and escapes the workspace name', () => {
    expect(terminalUrl('https://chloe.example.com', 'acme/dev', 100, 30)).toBe(
      'wss://chloe.example.com/api/v1/workspace-terminal/acme%2Fdev?cols=100&rows=30',
    );
    expect(terminalUrl('http://127.0.0.1:8080', 'api', 80, 24)).toBe(
      'ws://127.0.0.1:8080/api/v1/workspace-terminal/api?cols=80&rows=24',
    );
  });
});

describe('connectTerminal', () => {
  it('follows the frames from the gateway and relays bytes', () => {
    const { socket, states, output, connection } = setup();
    expect(socket.binaryType).toBe('arraybuffer');
    socket.open();
    socket.receive('{"type":"starting"}');
    socket.receive('{"type":"started"}');
    socket.receive(new TextEncoder().encode('$ ').buffer as ArrayBuffer);
    connection.send('ls\r');
    connection.resize(120, 40);
    socket.receive('{"type":"exit","code":0}');
    socket.drop();

    expect(states.map((s) => s.kind)).toEqual(['connecting', 'starting', 'running', 'exited']);
    expect(states.at(-1)).toEqual({ kind: 'exited', code: 0 });
    expect(output).toEqual(['$ ']);
    expect(new TextDecoder().decode(socket.sent[0] as Uint8Array)).toBe('ls\r');
    expect(socket.sent[1]).toBe('{"type":"resize","cols":120,"rows":40}');
  });

  it('explains a refused connection', () => {
    const { socket, states } = setup();
    socket.drop();
    expect(states.at(-1)?.kind).toBe('failed');
  });

  it('keeps the error from the gateway', () => {
    const { socket, states } = setup();
    socket.open();
    socket.receive('{"type":"error","message":"the workspace VM is not reachable"}');
    socket.drop();
    expect(states.at(-1)).toEqual({ kind: 'failed', message: 'the workspace VM is not reachable' });
  });

  it('reports a dropped shell and sends nothing after it', () => {
    const { socket, states, connection } = setup();
    socket.open();
    socket.receive('{"type":"started"}');
    socket.drop();
    connection.send('x');
    expect(states.at(-1)).toEqual({ kind: 'closed' });
    expect(socket.sent).toEqual([]);
  });
});
