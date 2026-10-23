// 管理 API の呼び出し。型は web/openapi.json から作る (`pnpm api`)。
import type { QueryClient } from '@tanstack/react-query';
import createFetchClient, { type Middleware } from 'openapi-fetch';
import createClient from 'openapi-react-query';

import type { components, paths } from './schema';

export type Schemas = components['schemas'];
export type ErrorBody = Schemas['ErrorBody'];

// セッションが切れたら、ログインの画面に戻す。
const redirectOnUnauthorized: Middleware = {
  onResponse({ response }) {
    if (response.status === 401 && !window.location.pathname.startsWith('/login')) {
      window.location.assign('/login');
    }
    return response;
  },
};

// 同じオリジンの /api を呼ぶ。絶対 URL にするのは、テスト (Node の fetch) でも同じ書き方で呼べるように。
export const fetchClient = createFetchClient<paths>({
  baseUrl: window.location.origin,
  credentials: 'same-origin',
});
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

// あるパスの問い合わせを、引数によらずすべて読み直させる。
export function invalidatePath(queryClient: QueryClient, path: keyof paths): Promise<void> {
  return queryClient.invalidateQueries({ predicate: (query) => query.queryKey[1] === path });
}
