import { createFileRoute } from '@tanstack/react-router';
import { z } from 'zod';

import { PoliciesPage, ScopePage } from '@/features/policies';

// 設定の層の一覧と、一つの層 (`?scope=org:acme`)。層の名前は `/` を含むので、検索パラメーターに置く。
export const Route = createFileRoute('/_app/policies')({
  validateSearch: z.object({ scope: z.string().min(1).optional() }),
  component: function Policies() {
    const { scope } = Route.useSearch();
    return scope ? <ScopePage key={scope} scope={scope} /> : <PoliciesPage />;
  },
});
