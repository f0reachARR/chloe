import { createFileRoute, redirect } from '@tanstack/react-router';

import { AppShell } from '@/app/shell';
import { meQuery } from '@/features/auth';
import { overlaySearchSchema } from '@/shared/overlays';

// ログインしたあとの画面の枠。セッションがなければログインの画面へ送る。重ねて開くもの (設定、依頼) の
// 検索パラメーターは、どの画面の上でも開けるよう、ここで受ける。
export const Route = createFileRoute('/_app')({
  validateSearch: overlaySearchSchema,
  beforeLoad: async ({ context }) => {
    try {
      await context.queryClient.ensureQueryData(meQuery());
    } catch {
      throw redirect({ to: '/login' });
    }
  },
  component: AppShell,
});
