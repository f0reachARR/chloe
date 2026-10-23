import { createFileRoute } from '@tanstack/react-router';
import { z } from 'zod';

import { LoginPage } from '@/features/auth';

export const Route = createFileRoute('/login/')({
  // GitHub のログインに失敗すると、ゲートウェイが ?error= を付けて戻す。
  validateSearch: z.object({ error: z.string().optional() }),
  component: function Login() {
    const { error } = Route.useSearch();
    return <LoginPage error={error} />;
  },
});
