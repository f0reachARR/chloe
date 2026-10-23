import '@mantine/core/styles.css';
import '@mantine/notifications/styles.css';

import { MantineProvider } from '@mantine/core';
import { Notifications } from '@mantine/notifications';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { Navigate, createBrowserRouter } from 'react-router';
import { RouterProvider } from 'react-router/dom';
import { registerSW } from 'virtual:pwa-register';

import { Inbox } from './routes/Inbox';
import { Layout } from './routes/Layout';
import { Login } from './routes/Login';
import { LoginLink } from './routes/LoginLink';
import { Workspaces } from './routes/Workspaces';
import { theme } from './theme';

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // 401 はログインの画面に戻すので、繰り返さない。
      retry: (count, error) =>
        count < 2 && !(error && typeof error === 'object' && 'error' in error),
      refetchOnWindowFocus: true,
    },
  },
});

const router = createBrowserRouter([
  { path: '/login', Component: Login },
  { path: '/login/link', Component: LoginLink },
  {
    path: '/',
    Component: Layout,
    children: [
      { index: true, element: <Navigate to="/inbox" replace /> },
      { path: 'inbox', Component: Inbox },
      { path: 'workspaces', Component: Workspaces },
      { path: '*', element: <Navigate to="/inbox" replace /> },
    ],
  },
]);

registerSW({ immediate: true });

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <MantineProvider theme={theme} defaultColorScheme="auto">
      <Notifications position="top-center" />
      <QueryClientProvider client={queryClient}>
        <RouterProvider router={router} />
      </QueryClientProvider>
    </MantineProvider>
  </StrictMode>,
);
