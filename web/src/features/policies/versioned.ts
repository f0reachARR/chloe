import { notifications } from '@mantine/notifications';
import { useMutation } from '@tanstack/react-query';
import { useState } from 'react';

import { errorMessage } from '@/shared/api/client';

// 版 (If-Match) が違って断られた。読んだあとに、ほかの保存や承認で書き換えられていた。
export class StaleError extends Error {}

// 置き換える API の結果 (openapi-fetch の形)。412 なら StaleError にする。
export function checkStale<T>({
  data,
  error,
  response,
}: {
  data?: T;
  error?: unknown;
  response: Response;
}): T | undefined {
  if (response.status === 412) throw new StaleError(errorMessage(error));
  if (error) throw error;
  return data;
}

// 読んでから置き換える設定の、版を使った保存。フォームを作ったときの版を持ち、読み直して版が
// 変わったときは、変えていなければ `reset` で合わせる (変えていれば合わせず、保存で 412 になる)。
// 412 を受けたら `stale` にし、最新を読むか、最新の版で上書きするかを利用者に選ばせる。
export function useVersionedSave<TData extends { version: string }, TPayload>({
  data,
  dirty,
  reset,
  put,
  refetch,
  onSent,
  savedMessage,
}: {
  data: TData;
  dirty: boolean;
  // フォームを `latest` に合わせる。
  reset: (latest: TData) => void;
  put: (version: string, payload: TPayload) => Promise<unknown>;
  refetch: () => Promise<TData>;
  // 送ったものを、すぐに保存したものとして扱う (読み直しを待つあいだも変更ありにしない)。
  onSent: (payload: TPayload) => void;
  savedMessage: string;
}) {
  const [version, setVersion] = useState(data.version);
  const [stale, setStale] = useState(false);
  const resetTo = (latest: TData) => {
    setVersion(latest.version);
    setStale(false);
    reset(latest);
  };
  const [seenVersion, setSeenVersion] = useState(data.version);
  if (data.version !== seenVersion) {
    setSeenVersion(data.version);
    if (!dirty && data.version !== version) resetTo(data);
  }

  const mutation = useMutation({
    mutationFn: ({ target, payload }: { target: string; payload: TPayload }) =>
      put(target, payload),
    onSuccess: async (_, { payload }) => {
      onSent(payload);
      setStale(false);
      notifications.show({ color: 'teal', message: savedMessage });
      resetTo(await refetch());
    },
    onError: (error) => {
      if (error instanceof StaleError) {
        setStale(true);
        // 最新の版を読んでおく (上書きするときに使う)。変えた欄はそのまま残る。
        void refetch();
        return;
      }
      notifications.show({ color: 'red', title: 'Could not save', message: errorMessage(error) });
    },
  });

  return {
    stale,
    saving: mutation.isPending,
    // フォームを作ったときの版で保存する。
    save: (payload: TPayload) => mutation.mutate({ target: version, payload }),
    // 412 のあと、最新の版で上書きする。
    saveOverLatest: (payload: TPayload) => mutation.mutate({ target: data.version, payload }),
    loadLatest: () => resetTo(data),
  };
}
