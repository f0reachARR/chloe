import type { QueryClient } from '@tanstack/react-query';

import { api, invalidatePath } from '@/shared/api/client';

// 依頼の一覧。`all` でなければ答えを待つものだけ。
export const asksQuery = (all = false) =>
  api.queryOptions('get', '/api/v1/asks', { params: { query: all ? { all } : {} } });

export const reloadAsks = (queryClient: QueryClient) => invalidatePath(queryClient, '/api/v1/asks');
