import type { QueryClient } from '@tanstack/react-query';
import { Navigate, Outlet, createRootRouteWithContext } from '@tanstack/react-router';

export interface RouterContext {
  queryClient: QueryClient;
}

export const Route = createRootRouteWithContext<RouterContext>()({
  component: Outlet,
  // 知らないパスは Inbox へ (ログインしていなければ、そこからログインの画面へ)。
  notFoundComponent: () => <Navigate to="/inbox" replace />,
});
