import { QueryClient } from '@tanstack/react-query';

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // 誤りの応答 (401 や 403 など) は、繰り返しても変わらないので繰り返さない。
      retry: (count, error) =>
        count < 2 && !(error && typeof error === 'object' && 'error' in error),
    },
  },
});
