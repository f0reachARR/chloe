import { useEffect, useState } from 'react';

import { type Listeners, type LiveState, connectEvents } from './stream';

// 画面を開いているあいだ出来事を受ける。`listeners` は変わるたびにつなぎ直すので、メモ化して渡す。
export function useEventStream(url: string, listeners: Listeners): LiveState {
  const [state, setState] = useState<LiveState>('connecting');
  useEffect(() => connectEvents({ url, listeners, onState: setState }), [url, listeners]);
  return state;
}
