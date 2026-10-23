import { Badge, Button, Card, Group, Stack, Text } from '@mantine/core';
import { IconTerminal2 } from '@tabler/icons-react';
import { Link } from '@tanstack/react-router';

import type { Schemas } from '@/shared/api/client';

import { agents } from './agents';

type WorkspaceInfo = Schemas['WorkspaceInfo'];

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
        Working: {agents(ws.busy)}
      </Badge>
    );
  }
  return (
    <Badge color="teal" variant="light">
      Running · idle
    </Badge>
  );
}

export function WorkspaceCard({ ws }: { ws: WorkspaceInfo }) {
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
        <Group gap="xs" justify="space-between">
          <AgentStatus ws={ws} />
          <Button
            // component={Link} では Link の検索パラメーターの型が失われるので、Link を直に描く。
            renderRoot={(props) => (
              <Link to="/terminal" search={{ workspace: ws.name }} {...props} />
            )}
            size="compact-sm"
            variant="light"
            leftSection={<IconTerminal2 size={14} />}
          >
            Terminal
          </Button>
        </Group>
        <Text size="xs" c="dimmed">
          {ws.vcpus} vCPU · {memory} · {ws.root} root · disk {ws.workspace_disk_gib} GiB
        </Text>
      </Stack>
    </Card>
  );
}
