import { Stack, Text } from '@mantine/core';
import { useQuery } from '@tanstack/react-query';

import { QueryState } from '@/shared/ui/QueryState';
import { Toolbar } from '@/shared/ui/Toolbar';

import { workspacesQuery } from './api';
import { WorkspaceCard } from './WorkspaceCard';

// 作業中かは、開いたときと読み直したときに VM に尋ねる (出来事では変わらないので、窓に戻った
// ときにも尋ね直さない)。
export function WorkspacesPage() {
  const workspaces = useQuery({ ...workspacesQuery(), refetchOnWindowFocus: false });
  return (
    <Stack>
      <Toolbar reloading={workspaces.isFetching} onReload={() => workspaces.refetch()}>
        <Text size="xs" c="dimmed">
          Agent status is checked when you reload.
        </Text>
      </Toolbar>
      <QueryState
        pending={workspaces.isPending}
        error={workspaces.error}
        empty={workspaces.data?.length === 0}
        emptyText="No workspaces yet. Connect over SSH to create your default one."
      />
      {workspaces.data?.map((ws) => (
        <WorkspaceCard key={ws.name} ws={ws} />
      ))}
    </Stack>
  );
}
