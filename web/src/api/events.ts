// 依頼の出来事 (`/api/v1/events` の SSE) を受け、一覧を読み直させる。
import { useQueryClient } from '@tanstack/react-query';
import { useEffect, useState } from 'react';
import { type AskEventData, reloadAsks } from './client';

export type LiveState = 'connecting' | 'live' | 'offline';

// 出来事を受けるあいだ、依頼とワークスペースの問い合わせを無効にして読み直させる。
// `onCreated` は新しい依頼が出たときに呼ぶ。
export function useAskEvents(onCreated: (event: AskEventData) => void): LiveState {
  const queryClient = useQueryClient();
  const [state, setState] = useState<LiveState>('connecting');

  useEffect(() => {
    let source: EventSource | undefined;
    let retry: number | undefined;
    let closed = false;

    const reload = () => void reloadAsks(queryClient);

    const connect = () => {
      setState('connecting');
      source = new EventSource('/api/v1/events');
      source.onopen = () => {
        setState('live');
        // つなぎ直すあいだに出た依頼を取りこぼさないよう、つながったら読み直す。
        reload();
      };
      source.addEventListener('ask', (message) => {
        const event = JSON.parse((message as MessageEvent<string>).data) as AskEventData;
        reload();
        if (event.type === 'created') {
          onCreated(event);
        }
      });
      source.addEventListener('lagged', reload);
      source.onerror = () => {
        // サーバーが閉じたとき (15 分ごと) はブラウザがつなぎ直す。断られた (401 など) ときは
        // 閉じるので、少し待ってから自分でつなぎ直す (セッションが切れていれば API の呼び出しで
        // ログインの画面に戻る)。
        if (source?.readyState === EventSource.CLOSED && !closed) {
          setState('offline');
          retry = window.setTimeout(connect, 5000);
        }
      };
    };

    connect();
    return () => {
      closed = true;
      source?.close();
      window.clearTimeout(retry);
    };
  }, [queryClient, onCreated]);

  return state;
}
