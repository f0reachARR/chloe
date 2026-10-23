import type { QueryClient } from '@tanstack/react-query';

import { api, invalidatePath } from '@/shared/api/client';

// 作業中かは VM に尋ねる (2 秒まで) ので、ルートの読み込みでは待たない。
export const workspacesQuery = () =>
  api.queryOptions('get', '/api/v1/workspaces', { params: { query: { status: true } } });

export const reloadWorkspaces = (queryClient: QueryClient) =>
  invalidatePath(queryClient, '/api/v1/workspaces');
