import { Alert, Button, Checkbox, Stack, Text, TextInput } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconTrash } from '@tabler/icons-react';
import { useQueryClient } from '@tanstack/react-query';
import { useNavigate } from '@tanstack/react-router';
import { useState } from 'react';

import { api, errorMessage } from '@/shared/api/client';
import type { WorkspaceSettingsSectionProps } from '@/shared/feature';

import { reloadWorkspaces } from './api';

// ワークスペースを消す。元に戻せないので、名前を打ち込ませて確かめる。消せるのは、SSH の
// `chloe ws rm` と同じく、そのワークスペースに入れる人である。
export function DeleteSection({ workspace }: WorkspaceSettingsSectionProps) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const [typed, setTyped] = useState('');
  const [disks, setDisks] = useState(false);
  const remove = api.useMutation('delete', '/api/v1/workspaces/{name}', {
    onError: (error) =>
      notifications.show({ color: 'red', title: 'Could not delete', message: errorMessage(error) }),
  });

  const submit = () =>
    remove.mutate(
      { params: { path: { name: workspace }, query: disks ? { disks: true } : {} } },
      {
        onSuccess: () => {
          notifications.show({ color: 'teal', message: `Deleted ${workspace}` });
          // 端末など、消したワークスペースの画面にいても一覧へ移る。
          void navigate({ to: '/workspaces', search: {} });
          void reloadWorkspaces(queryClient);
        },
      },
    );

  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        if (typed === workspace) submit();
      }}
    >
      <Alert color="red" title="Delete this workspace" icon={<IconTrash size={16} />}>
        <Stack gap="sm">
          <Text size="sm">
            This cannot be undone. A running workspace is stopped first, which closes open terminals
            and stops running agents. Its rules and published ports are removed too.
          </Text>
          <Checkbox
            label="Also delete the disks"
            description="Otherwise the workspace and Docker disks stay on the runner host."
            checked={disks}
            onChange={(event) => setDisks(event.currentTarget.checked)}
          />
          <TextInput
            label={
              <>
                Type{' '}
                <Text span ff="monospace" fw={600}>
                  {workspace}
                </Text>{' '}
                to confirm
              </>
            }
            autoComplete="off"
            value={typed}
            onChange={(event) => setTyped(event.currentTarget.value)}
          />
          <Button
            type="submit"
            color="red"
            disabled={typed !== workspace}
            loading={remove.isPending}
            leftSection={<IconTrash size={14} />}
          >
            Delete workspace
          </Button>
        </Stack>
      </Alert>
    </form>
  );
}
