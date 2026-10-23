import { useQueryClient } from '@tanstack/react-query';
import { useNavigate } from '@tanstack/react-router';
import { useMemo } from 'react';

import { type Listeners, type LiveState, useEventStream } from '@/shared/events';
import type { FeatureContext } from '@/shared/feature';

import { features } from './features';

// 各機能が申告した出来事の処理を集め、一本の接続で受ける。
export function useFeatureEvents(): LiveState {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const listeners = useMemo(() => {
    const context: FeatureContext = { queryClient, navigate: (to) => void navigate({ to }) };
    const collected: Listeners = {};
    for (const feature of features) {
      for (const [name, handler] of Object.entries(feature.events ?? {})) {
        (collected[name] ??= []).push((data) => handler(data, context));
      }
    }
    return collected;
  }, [queryClient, navigate]);
  return useEventStream('/api/v1/events', listeners);
}
