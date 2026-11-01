import { Button } from '@mantine/core';
import { IconInbox } from '@tabler/icons-react';
import { Link } from '@tanstack/react-router';

import { openOverlay } from '@/shared/overlays';

import { useWorkspacePendingAsks } from './WorkspaceAsks';

// 一つのワークスペースの依頼を、今の画面の上に開くボタン。答えを待つ数を出す。
export function WorkspaceAsksButton({ workspace }: { workspace: string }) {
  const pending = useWorkspacePendingAsks(workspace);
  return (
    <Button
      renderRoot={(props) => <Link to="." search={openOverlay('asks', workspace)} {...props} />}
      size="compact-sm"
      variant={pending > 0 ? 'filled' : 'subtle'}
      leftSection={<IconInbox size={14} />}
      aria-label={pending > 0 ? `${pending} asks waiting` : 'Asks'}
    >
      {pending > 0 ? pending : null}
    </Button>
  );
}
