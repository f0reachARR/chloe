import { ActionIcon, Button, Group, Tooltip } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconExternalLink, IconX } from '@tabler/icons-react';
import { useQuery, useQueryClient } from '@tanstack/react-query';

import { api, errorMessage } from '@/shared/api/client';

import { portsQuery, reloadPorts } from './api';

// VM の中で `chloe-ask port publish` で公開したポートへのリンク。開くと、プロキシのホストで
// Web の画面のログインを通って VM の中のサーバーに届く。公開は VM の中からしかできない。
export function WorkspacePorts({ workspace }: { workspace: string }) {
  const queryClient = useQueryClient();
  const ports = useQuery({ ...portsQuery(workspace), refetchOnWindowFocus: false });
  const unpublish = api.useMutation('delete', '/api/v1/workspace-ports/{name}', {
    onError: (error) => {
      notifications.show({
        color: 'red',
        title: 'Could not unpublish',
        message: errorMessage(error),
      });
    },
    onSettled: () => reloadPorts(queryClient),
  });
  if (!ports.data?.length) return null;
  return (
    <Group gap="xs">
      {ports.data.map((port) => (
        <Group key={port.port} gap={2} wrap="nowrap">
          <Button
            component="a"
            href={port.url ?? undefined}
            target="_blank"
            rel="noopener noreferrer"
            disabled={!port.url}
            size="compact-sm"
            variant="default"
            rightSection={<IconExternalLink size={14} />}
          >
            {port.port}
            {port.scheme === 'https' ? ' (https)' : ''}
          </Button>
          <Tooltip label={`Unpublish port ${port.port}`}>
            <ActionIcon
              aria-label={`Unpublish port ${port.port}`}
              variant="subtle"
              color="gray"
              size="sm"
              loading={unpublish.isPending && unpublish.variables?.params.query.port === port.port}
              onClick={() =>
                unpublish.mutate({
                  params: { path: { name: workspace }, query: { port: port.port } },
                })
              }
            >
              <IconX size={14} />
            </ActionIcon>
          </Tooltip>
        </Group>
      ))}
    </Group>
  );
}
