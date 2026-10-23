import { Alert, Badge, Button, Card, Group, Loader, Stack, Text } from '@mantine/core';
import { IconRefresh } from '@tabler/icons-react';
import { Link } from 'react-router';

import { type WorkspaceInfo, api, errorMessage } from '../api/client';

// 作業中かは、開いたときと読み直したときに VM に尋ねる (出来事では変わらないので、自動では
// 読み直さない)。
export function Workspaces() {
  const workspaces = api.useQuery(
    'get',
    '/api/v1/workspaces',
    { params: { query: { status: true } } },
    { refetchOnWindowFocus: false },
  );

  return (
    <Stack>
      <Group justify="space-between" wrap="nowrap">
        <Text size="xs" c="dimmed">
          Agent status is checked when you reload.
        </Text>
        <Button
          variant="subtle"
          size="compact-sm"
          leftSection={<IconRefresh size={14} />}
          loading={workspaces.isFetching}
          onClick={() => workspaces.refetch()}
        >
          Reload
        </Button>
      </Group>
      {workspaces.isPending && (
        <Group justify="center" py="xl">
          <Loader />
        </Group>
      )}
      {workspaces.error && <Alert color="red">{errorMessage(workspaces.error)}</Alert>}
      {workspaces.data?.length === 0 && (
        <Text c="dimmed" ta="center" py="xl">
          No workspaces yet. Connect over SSH to create your default one.
        </Text>
      )}
      {workspaces.data?.map((ws) => (
        <WorkspaceCard key={ws.name} ws={ws} />
      ))}
    </Stack>
  );
}

function AgentStatus({ ws }: { ws: WorkspaceInfo }) {
  if (!ws.running) {
    return (
      <Badge color="gray" variant="light">
        Stopped
      </Badge>
    );
  }
  if (ws.busy === null) {
    return (
      <Badge color="gray" variant="outline">
        Running · status unknown
      </Badge>
    );
  }
  if (ws.busy && ws.busy.length > 0) {
    return (
      <Badge color="orange" variant="light">
        Working: {ws.busy.join(', ')}
      </Badge>
    );
  }
  return (
    <Badge color="teal" variant="light">
      Running · idle
    </Badge>
  );
}

function WorkspaceCard({ ws }: { ws: WorkspaceInfo }) {
  const memory =
    ws.memory_mib >= 1024 ? `${(ws.memory_mib / 1024).toFixed(0)} GiB` : `${ws.memory_mib} MiB`;
  return (
    <Card withBorder padding="md">
      <Stack gap="xs">
        <Group justify="space-between" wrap="nowrap">
          <Text fw={600} ff="monospace" truncate>
            {ws.name}
          </Text>
          {ws.pending_asks > 0 && (
            <Badge component={Link} to="/inbox" color="blue" style={{ cursor: 'pointer' }}>
              {ws.pending_asks} waiting
            </Badge>
          )}
        </Group>
        <Group gap="xs">
          <AgentStatus ws={ws} />
        </Group>
        <Text size="xs" c="dimmed">
          {ws.vcpus} vCPU · {memory} · {ws.root} root · disk {ws.workspace_disk_gib} GiB
        </Text>
      </Stack>
    </Card>
  );
}
