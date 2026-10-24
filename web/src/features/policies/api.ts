import type { QueryClient } from '@tanstack/react-query';

import { api, invalidatePath } from '@/shared/api/client';

// ワークスペースで効く規則、ワークスペース自身の規則とその版、変えられるか。
export const rulesQuery = (workspace: string) =>
  api.queryOptions('get', '/api/v1/workspace-rules/{name}', {
    params: { path: { name: workspace } },
  });

export const reloadRules = (queryClient: QueryClient) =>
  invalidatePath(queryClient, '/api/v1/workspace-rules/{name}');
