import { createFileRoute } from '@tanstack/react-router';
import { z } from 'zod';

import { WorkspaceAsksButton } from '@/features/inbox';
import { TerminalPage } from '@/features/terminal';

// ワークスペースの名前は `/` を含むので (acme/dev/tools)、パスではなく検索パラメーターに置く。
export const Route = createFileRoute('/_app/terminal')({
  validateSearch: z.object({ workspace: z.string().min(1) }),
  staticData: { wide: true },
  component: function Terminal() {
    const { workspace } = Route.useSearch();
    // ワークスペースを替えたら、端末を作り直す。
    // 依頼は端末の上に重ねて開くので、端末を離れずに答えられる。
    return (
      <TerminalPage
        key={workspace}
        workspace={workspace}
        actions={<WorkspaceAsksButton workspace={workspace} />}
      />
    );
  },
});
