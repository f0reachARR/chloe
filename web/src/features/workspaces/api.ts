import type { QueryClient } from '@tanstack/react-query';

import { api, invalidatePath } from '@/shared/api/client';

// 作業中かは VM に尋ねる (2 秒まで) ので、ルートの読み込みでは待たない。
export const workspacesQuery = () =>
  api.queryOptions('get', '/api/v1/workspaces', { params: { query: { status: true } } });

// 一つのワークスペースの設定と、動いているか (VM には尋ねない)。
export const workspaceQuery = (workspace: string) =>
  api.queryOptions('get', '/api/v1/workspaces/{name}', {
    params: { path: { name: workspace } },
  });

export const reloadWorkspaces = (queryClient: QueryClient) =>
  Promise.all([
    invalidatePath(queryClient, '/api/v1/workspaces'),
    invalidatePath(queryClient, '/api/v1/workspaces/{name}'),
  ]);

export const portsQuery = (workspace: string) =>
  api.queryOptions('get', '/api/v1/workspace-ports/{name}', {
    params: { path: { name: workspace } },
  });

export const reloadPorts = (queryClient: QueryClient) =>
  invalidatePath(queryClient, '/api/v1/workspace-ports/{name}');
