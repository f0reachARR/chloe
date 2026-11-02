import { createFileRoute } from '@tanstack/react-router';
import { z } from 'zod';

import { SecretsPage } from '@/features/secrets';

// 持ち主ごとのシークレット (`?owner=org:acme`)。持ち主の名前は `/` を含むので、検索パラメーターに置く。
export const Route = createFileRoute('/_app/secrets')({
  validateSearch: z.object({ owner: z.string().min(1).optional() }),
  component: function Secrets() {
    const { owner } = Route.useSearch();
    const navigate = Route.useNavigate();
    return (
      <SecretsPage
        owner={owner}
        onOwnerChange={(next) => void navigate({ search: (prev) => ({ ...prev, owner: next }) })}
      />
    );
  },
});
