import { createRouter } from '@tanstack/react-router';

import { routeTree } from '../routeTree.gen';
import { queryClient } from './query';

export const router = createRouter({
  routeTree,
  context: { queryClient },
  // 読み込んだものの鮮度は TanStack Query に任せる。
  defaultPreloadStaleTime: 0,
  defaultPreload: 'intent',
});

declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router;
  }
  // ルートが枠に申告するもの。
  interface StaticDataRouteOption {
    // 画面の幅いっぱいに描く (端末など)。ほかの画面は読みやすい幅に収める。
    wide?: boolean;
  }
}
