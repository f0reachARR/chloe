import { createFileRoute } from '@tanstack/react-router';
import { z } from 'zod';

import { InboxPage, asksQuery } from '@/features/inbox';

export const Route = createFileRoute('/_app/inbox')({
  validateSearch: z.object({ all: z.boolean().optional() }),
  loaderDeps: ({ search }) => ({ all: search.all ?? false }),
  loader: ({ context, deps }) => context.queryClient.ensureQueryData(asksQuery(deps.all)),
  component: function Inbox() {
    const { all = false } = Route.useSearch();
    const navigate = Route.useNavigate();
    return (
      <InboxPage
        all={all}
        onAllChange={(next) => navigate({ search: next ? { all: true } : {}, replace: true })}
      />
    );
  },
});
