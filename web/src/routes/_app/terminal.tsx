import { createFileRoute } from '@tanstack/react-router';
import { z } from 'zod';

import { TerminalPage } from '@/features/terminal';

// ワークスペースの名前は `/` を含むので (acme/dev/tools)、パスではなく検索パラメーターに置く。
export const Route = createFileRoute('/_app/terminal')({
  validateSearch: z.object({ workspace: z.string().min(1) }),
  staticData: { wide: true },
  component: function Terminal() {
    const { workspace } = Route.useSearch();
    // ワークスペースを替えたら、端末を作り直す。
    return <TerminalPage key={workspace} workspace={workspace} />;
  },
});
