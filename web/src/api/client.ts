// 管理 API の呼び出し。型は web/openapi.json から作る (`pnpm api`)。
import type { QueryClient } from '@tanstack/react-query';
import createFetchClient, { type Middleware } from 'openapi-fetch';
import createClient from 'openapi-react-query';
import type { components, paths } from './schema';

export type WorkspaceInfo = components['schemas']['WorkspaceInfo'];
export type AskInfo = components['schemas']['AskInfo'];
export type AskEventData = components['schemas']['AskEventData'];
export type AuthMethods = components['schemas']['AuthMethods'];
export type ErrorBody = components['schemas']['ErrorBody'];

// セッションが切れたら、ログインの画面に戻す。
const redirectOnUnauthorized: Middleware = {
  onResponse({ response }) {
    if (response.status === 401 && !window.location.pathname.startsWith('/login')) {
      window.location.assign('/login');
    }
    return response;
  },
};

export const fetchClient = createFetchClient<paths>({ credentials: 'same-origin' });
fetchClient.use(redirectOnUnauthorized);

export const api = createClient(fetchClient);

// 誤りの応答や例外から、人に見せる言葉を取り出す。
export function errorMessage(error: unknown): string {
  if (error && typeof error === 'object' && 'error' in error) {
    return String((error as ErrorBody).error);
  }
  if (error instanceof Error) {
    return error.message;
  }
  return 'Something went wrong';
}

// 依頼とワークスペースの一覧を読み直させる (依頼が出たり答えたりしたとき)。
export function reloadAsks(queryClient: QueryClient): Promise<void> {
  return queryClient.invalidateQueries({
    predicate: (query) => {
      const path = query.queryKey[1];
      return path === '/api/v1/asks' || path === '/api/v1/workspaces';
    },
  });
}
