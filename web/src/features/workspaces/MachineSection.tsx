import {
  Alert,
  Badge,
  Button,
  Divider,
  Group,
  NumberInput,
  SegmentedControl,
  Select,
  SimpleGrid,
  Stack,
  Text,
  TextInput,
} from '@mantine/core';
import { useForm } from '@mantine/form';
import { notifications } from '@mantine/notifications';
import { IconPlayerStop } from '@tabler/icons-react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useEffect, useState } from 'react';

import { type Schemas, api, errorMessage } from '@/shared/api/client';
import type { WorkspaceSettingsSectionProps } from '@/shared/feature';
import { QueryState } from '@/shared/ui/QueryState';

import { reloadWorkspaces, workspaceQuery } from './api';
import { type MachineValues, idlePattern, needsRestart, toPatch, toValues } from './machine';

const nextStart = 'Applies from the next start.';

// 止める操作。止めると、開いている端末とエージェントも止まる。起動は端末やポートを開いたときに起きる。
function useStopWorkspace(workspace: string) {
  const queryClient = useQueryClient();
  return api.useMutation('patch', '/api/v1/workspaces/{name}', {
    onSuccess: () => notifications.show({ color: 'teal', message: `Stopped ${workspace}` }),
    onError: (error) =>
      notifications.show({ color: 'red', title: 'Could not stop', message: errorMessage(error) }),
    onSettled: () => reloadWorkspaces(queryClient),
  });
}

function Power({ workspace, running }: { workspace: string; running: boolean }) {
  const stop = useStopWorkspace(workspace);
  const [confirming, setConfirming] = useState(false);
  return (
    <Stack gap="xs">
      <Group justify="space-between">
        <Group gap="xs">
          <Text fw={500}>Power</Text>
          <Badge color={running ? 'teal' : 'gray'} variant="light">
            {running ? 'Running' : 'Stopped'}
          </Badge>
        </Group>
        {running && !confirming && (
          <Button
            color="red"
            variant="light"
            size="compact-sm"
            leftSection={<IconPlayerStop size={14} />}
            onClick={() => setConfirming(true)}
          >
            Stop
          </Button>
        )}
      </Group>
      {running && confirming && (
        <Alert color="red" title="Stop this workspace?">
          <Text size="sm" mb="xs">
            Open terminals and running agents stop too. It starts again when you open a terminal or
            a published port.
          </Text>
          <Group gap="xs">
            <Button
              size="compact-sm"
              color="red"
              loading={stop.isPending}
              onClick={() =>
                stop.mutate(
                  { params: { path: { name: workspace } }, body: { running: false } },
                  { onSettled: () => setConfirming(false) },
                )
              }
            >
              Stop now
            </Button>
            <Button size="compact-sm" variant="default" onClick={() => setConfirming(false)}>
              Cancel
            </Button>
          </Group>
        </Alert>
      )}
      {!running && (
        <Text size="sm" c="dimmed">
          It starts when you open a terminal or a published port.
        </Text>
      )}
    </Stack>
  );
}

function MachineForm({
  info,
  onDirtyChange,
}: {
  info: Schemas['WorkspaceInfo'];
  onDirtyChange: (dirty: boolean) => void;
}) {
  const queryClient = useQueryClient();
  const workspace = info.name;
  const [initial, setInitial] = useState(() => toValues(info));
  // 保存したあと、次の起動から効く項目を変えていて VM が動いていれば、止めるかを尋ねる。
  const [restartPending, setRestartPending] = useState(false);
  const form = useForm<MachineValues>({
    initialValues: initial,
    validate: {
      vcpus: (v) => (Number.isInteger(v) && v >= 1 && v <= 32 ? null : 'Use 1 to 32 vCPUs'),
      memoryGib: (v) => (v >= 0.5 ? null : 'Use at least 0.5 GiB'),
      workspaceDiskGib: (v) => (v >= initial.workspaceDiskGib ? null : 'Disks can only grow'),
      dockerDiskGib: (v) => (v >= initial.dockerDiskGib ? null : 'Disks can only grow'),
      idleAfter: (v, values) =>
        values.idle !== 'after' || idlePattern.test(v) ? null : 'Use e.g. 30m, 2h or 1d',
    },
  });
  const patch = toPatch(initial, form.values);
  const dirty = Object.keys(patch).length > 0;
  useEffect(() => onDirtyChange(dirty), [dirty, onDirtyChange]);

  const save = api.useMutation('patch', '/api/v1/workspaces/{name}', {
    onError: (error) =>
      notifications.show({ color: 'red', title: 'Could not save', message: errorMessage(error) }),
    onSettled: () => reloadWorkspaces(queryClient),
  });
  const stop = useStopWorkspace(workspace);

  const submit = form.onSubmit(() => {
    const body = patch;
    save.mutate(
      { params: { path: { name: workspace } }, body },
      {
        onSuccess: (saved) => {
          const values = toValues(saved);
          setInitial(values);
          form.setInitialValues(values);
          form.setValues(values);
          setRestartPending(needsRestart(body) && saved.running);
          notifications.show({ color: 'teal', message: `Saved ${workspace}` });
        },
      },
    );
  });

  return (
    <form onSubmit={submit}>
      <Stack>
        <Power workspace={workspace} running={info.running} />
        {restartPending && info.running && (
          <Alert color="blue" title="Restart to apply">
            <Text size="sm" mb="xs">
              The new size, root or disks apply from the next start. Stopping closes open terminals;
              the workspace starts again when you open one.
            </Text>
            <Group gap="xs">
              <Button
                size="compact-sm"
                loading={stop.isPending}
                onClick={() =>
                  stop.mutate(
                    { params: { path: { name: workspace } }, body: { running: false } },
                    { onSuccess: () => setRestartPending(false) },
                  )
                }
              >
                Stop now
              </Button>
              <Button size="compact-sm" variant="default" onClick={() => setRestartPending(false)}>
                Later
              </Button>
            </Group>
          </Alert>
        )}
        <Divider />
        <SimpleGrid cols={{ base: 1, xs: 2 }}>
          <NumberInput
            label="vCPUs"
            description={nextStart}
            min={1}
            max={32}
            allowDecimal={false}
            {...form.getInputProps('vcpus')}
          />
          <NumberInput
            label="Memory (GiB)"
            description={nextStart}
            min={0.5}
            step={1}
            decimalScale={1}
            {...form.getInputProps('memoryGib')}
          />
          <NumberInput
            label="Workspace disk (GiB)"
            description="Grows from the next start."
            min={initial.workspaceDiskGib}
            allowDecimal={false}
            {...form.getInputProps('workspaceDiskGib')}
          />
          <NumberInput
            label="Docker disk (GiB)"
            description="Grows from the next start."
            min={initial.dockerDiskGib}
            allowDecimal={false}
            {...form.getInputProps('dockerDiskGib')}
          />
        </SimpleGrid>
        <Stack gap={4}>
          <Text size="sm" fw={500}>
            Root
          </Text>
          <Text size="xs" c="dimmed">
            Ephemeral resets everything outside the workspace disk on each start. {nextStart}
          </Text>
          <SegmentedControl
            data={[
              { value: 'ephemeral', label: 'Ephemeral' },
              { value: 'persistent', label: 'Persistent' },
            ]}
            {...form.getInputProps('root')}
          />
        </Stack>
        <SimpleGrid cols={{ base: 1, xs: 2 }}>
          <Select
            label="Idle stop"
            description="When to stop after the last connection closes."
            allowDeselect={false}
            data={[
              { value: 'after', label: 'After a while' },
              { value: 'never', label: 'Never' },
              { value: 'default', label: "Gateway's default" },
            ]}
            {...form.getInputProps('idle')}
          />
          {form.values.idle === 'after' && (
            <TextInput
              label="Idle time"
              description="e.g. 30m, 2h or 1d. Applies now."
              {...form.getInputProps('idleAfter')}
            />
          )}
        </SimpleGrid>
        <Group justify="flex-end" gap="xs">
          <Button
            variant="default"
            disabled={!dirty || save.isPending}
            onClick={() => form.setValues(initial)}
          >
            Reset
          </Button>
          <Button type="submit" disabled={!dirty} loading={save.isPending}>
            Save
          </Button>
        </Group>
      </Stack>
    </form>
  );
}

// VM の大きさ、root、ディスク、止めるまでの時間と、止める操作。今効いている値を出し、変えた項目だけを送る。
export function MachineSection({ workspace, onDirtyChange }: WorkspaceSettingsSectionProps) {
  const info = useQuery(workspaceQuery(workspace));
  if (!info.data) {
    return <QueryState pending={info.isPending} error={info.error} empty={false} emptyText="" />;
  }
  // 動いているかは読み直すたびに変わるが、フォームの値は保存したときにだけ読み直す。
  return <MachineForm info={info.data} onDirtyChange={onDirtyChange} />;
}
