// 端末の WebSocket (`/api/v1/workspace-terminal/{name}`)。バイナリのフレームは端末の入力と出力、
// テキストのフレームは JSON (送るのは ClientFrame、受けるのは ServerFrame)。
// 切れたらシェルは終わるので (SSH と同じ)、つなぎ直しはしない。開き直すかは利用者が決める。
// 何も打たないあいだに途中のリバースプロキシに切られないよう、ping のフレームを送り続ける
// (ブラウザからは制御フレームの Ping を送れない)。
import type { Schemas } from '@/shared/api/client';

type ClientFrame = Schemas['ClientFrame'];
type ServerFrame = Schemas['ServerFrame'];

// ping のフレームを送る間隔。nginx などの既定の待ち時間 (60 秒) より短くする。
export const PING_INTERVAL_MS = 30_000;

export type TerminalState =
  | { kind: 'connecting' }
  // ワークスペースを用意している (止まっていれば VM を起動する)。
  | { kind: 'starting' }
  | { kind: 'running' }
  | { kind: 'exited'; code: number }
  | { kind: 'failed'; message: string }
  // シェルが終わる前に接続が切れた。
  | { kind: 'closed' };

export interface TerminalConnection {
  send(data: string | Uint8Array): void;
  resize(cols: number, rows: number): void;
  close(): void;
}

// ワークスペースの名前の `/` は `%2F` にする (管理 API のほかの経路と同じ)。
export function terminalUrl(origin: string, workspace: string, cols: number, rows: number): string {
  const url = new URL(`/api/v1/workspace-terminal/${encodeURIComponent(workspace)}`, origin);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  url.search = new URLSearchParams({ cols: String(cols), rows: String(rows) }).toString();
  return url.toString();
}

export function connectTerminal({
  url,
  onOutput,
  onState,
  createSocket = (target) => new WebSocket(target),
}: {
  url: string;
  onOutput: (data: Uint8Array) => void;
  onState: (state: TerminalState) => void;
  createSocket?: (url: string) => WebSocket;
}): TerminalConnection {
  const encoder = new TextEncoder();
  const socket = createSocket(url);
  socket.binaryType = 'arraybuffer';
  let state: TerminalState = { kind: 'connecting' };
  const setState = (next: TerminalState) => {
    state = next;
    onState(next);
  };
  // 終わったあと (exit か失敗のあと) は、状態を変えない。
  const ended = () => state.kind === 'exited' || state.kind === 'failed' || state.kind === 'closed';
  onState(state);

  let pinger: ReturnType<typeof setInterval> | undefined;
  const stopPinging = () => clearInterval(pinger);
  socket.addEventListener('open', () => {
    pinger = setInterval(() => {
      if (open()) {
        const frame: ClientFrame = { type: 'ping' };
        socket.send(JSON.stringify(frame));
      }
    }, PING_INTERVAL_MS);
  });

  socket.addEventListener('message', (event: MessageEvent) => {
    if (typeof event.data !== 'string') {
      if (state.kind === 'starting' || state.kind === 'connecting') {
        setState({ kind: 'running' });
      }
      onOutput(new Uint8Array(event.data as ArrayBuffer));
      return;
    }
    let frame: ServerFrame;
    try {
      frame = JSON.parse(event.data) as ServerFrame;
    } catch {
      return;
    }
    switch (frame.type) {
      case 'starting':
        setState({ kind: 'starting' });
        break;
      case 'started':
        setState({ kind: 'running' });
        break;
      case 'exit':
        setState({ kind: 'exited', code: frame.code });
        break;
      case 'error':
        setState({ kind: 'failed', message: frame.message });
        break;
    }
  });
  socket.addEventListener('close', () => {
    stopPinging();
    if (ended()) {
      return;
    }
    // 切り替える前に断られた (ワークスペースがない、入れない、ログインが切れた)。ブラウザは
    // 断られた理由を見せないので、思い当たるものを伝える。
    if (state.kind === 'connecting') {
      setState({
        kind: 'failed',
        message:
          'Could not open the terminal. Check that the workspace exists and that you are signed in.',
      });
      return;
    }
    setState({ kind: 'closed' });
  });

  const open = () => socket.readyState === WebSocket.OPEN;
  return {
    send(data) {
      if (open()) {
        socket.send(typeof data === 'string' ? encoder.encode(data) : data);
      }
    },
    resize(cols, rows) {
      if (open()) {
        const frame: ClientFrame = { type: 'resize', cols, rows };
        socket.send(JSON.stringify(frame));
      }
    },
    close() {
      stopPinging();
      socket.close();
    },
  };
}
