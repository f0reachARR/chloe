// 出来事 (`/api/v1/events` の SSE) を一本の接続で受け、名前ごとに登録した処理に配る。
// ブラウザは同じサーバーへの接続の数に上限があるので、機能ごとに接続を張らない。

export type LiveState = 'connecting' | 'live' | 'offline';

export type Listener = (data: unknown) => void;

// 出来事の名前 → 処理。`resync` は、つながったときと、サーバーが取りこぼしを知らせた (`lagged`)
// ときに呼ぶ。
export type Listeners = Record<string, Listener[]>;

export const RESYNC = 'resync';

export interface StreamOptions {
  url: string;
  listeners: Listeners;
  onState: (state: LiveState) => void;
  // 断られて閉じたあと、つなぎ直すまでの時間。
  retryMs?: number;
  // テストで差し替える。
  createSource?: (url: string) => EventSource;
}

export function connectEvents({
  url,
  listeners,
  onState,
  retryMs = 5000,
  createSource = (u) => new EventSource(u),
}: StreamOptions): () => void {
  let source: EventSource | undefined;
  let retry: ReturnType<typeof setTimeout> | undefined;
  let closed = false;

  const dispatch = (name: string, data: unknown) => {
    for (const listener of listeners[name] ?? []) {
      listener(data);
    }
  };

  const connect = () => {
    onState('connecting');
    source = createSource(url);
    source.addEventListener('open', () => {
      onState('live');
      // つなぎ直すあいだに出た出来事を取りこぼさないよう、つながったら読み直させる。
      dispatch(RESYNC, undefined);
    });
    for (const name of Object.keys(listeners)) {
      if (name === RESYNC) continue;
      source.addEventListener(name, (message) => {
        dispatch(name, parse((message as MessageEvent<string>).data));
      });
    }
    source.addEventListener('lagged', () => dispatch(RESYNC, undefined));
    source.addEventListener('error', () => {
      // サーバーが閉じたとき (15 分ごと) はブラウザがつなぎ直す。断られた (401 など) ときは
      // 閉じるので、少し待ってから自分でつなぎ直す。
      if (source?.readyState === EventSource.CLOSED && !closed) {
        onState('offline');
        retry = setTimeout(connect, retryMs);
      }
    });
  };

  connect();
  return () => {
    closed = true;
    source?.close();
    clearTimeout(retry);
  };
}

function parse(data: string): unknown {
  try {
    return JSON.parse(data);
  } catch {
    return data;
  }
}
