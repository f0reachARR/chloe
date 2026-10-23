import { createFileRoute } from '@tanstack/react-router';
import { z } from 'zod';

import { LinkPage } from '@/features/auth';

export const Route = createFileRoute('/login/link')({
  validateSearch: z.object({ code: z.string().optional() }),
  component: function Link() {
    const { code } = Route.useSearch();
    return <LinkPage code={code} />;
  },
});
