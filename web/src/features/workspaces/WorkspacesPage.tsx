import { Button, Group, Stack, Text } from '@mantine/core';
import { IconPlus } from '@tabler/icons-react';
import { useQuery } from '@tanstack/react-query';
import { useEffect, useState } from 'react';

import { QueryState } from '@/shared/ui/QueryState';
import { Toolbar } from '@/shared/ui/Toolbar';

import { workspacesQuery } from './api';
import { CreateWorkspace } from './CreateWorkspace';
import { WorkspaceCard, workspaceCardId } from './WorkspaceCard';

// 作業中かは、開いたときと読み直したときに VM に尋ねる (出来事では変わらないので、窓に戻った
// ときにも尋ね直さない)。
export function WorkspacesPage() {
  const workspaces = useQuery({ ...workspacesQuery(), refetchOnWindowFocus: false });
  const [creating, setCreating] = useState(false);
  // 作ったワークスペース。一覧に現れたら、そのカードまで動かす。
  const [created, setCreated] = useState<string>();
  const shown = workspaces.data?.some((ws) => ws.name === created) ?? false;
  useEffect(() => {
    if (!created || !shown) return;
    document.getElementById(workspaceCardId(created))?.scrollIntoView?.({ block: 'nearest' });
  }, [created, shown]);

  return (
    <Stack>
      <Toolbar reloading={workspaces.isFetching} onReload={() => workspaces.refetch()}>
        <Group gap="sm" wrap="nowrap">
          <Button
            size="compact-sm"
            leftSection={<IconPlus size={14} />}
            onClick={() => setCreating(true)}
          >
            New workspace
          </Button>
          <Text size="xs" c="dimmed" visibleFrom="xs">
            Agent status is checked when you reload.
          </Text>
        </Group>
      </Toolbar>
      <QueryState
        pending={workspaces.isPending}
        error={workspaces.error}
        empty={workspaces.data?.length === 0}
        emptyText="No workspaces yet. Create one, or connect over SSH to create your default one."
      />
      {workspaces.data?.map((ws) => (
        <WorkspaceCard key={ws.name} ws={ws} highlighted={ws.name === created} />
      ))}
      <CreateWorkspace
        opened={creating}
        onClose={() => setCreating(false)}
        onCreated={(name) => {
          setCreating(false);
          setCreated(name);
        }}
      />
    </Stack>
  );
}
