import { createFileRoute, redirect } from '@tanstack/react-router';

import { AppShell } from '@/app/shell';
import { meQuery } from '@/features/auth';

// ログインしたあとの画面の枠。セッションがなければログインの画面へ送る。
export const Route = createFileRoute('/_app')({
  beforeLoad: async ({ context }) => {
    try {
      await context.queryClient.ensureQueryData(meQuery());
    } catch {
      throw redirect({ to: '/login' });
    }
  },
  component: AppShell,
});
