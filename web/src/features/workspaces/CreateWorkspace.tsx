import {
  Accordion,
  Button,
  Group,
  Modal,
  NumberInput,
  SegmentedControl,
  Select,
  SimpleGrid,
  Stack,
  Text,
  TextInput,
} from '@mantine/core';
import { useForm } from '@mantine/form';
import { useMediaQuery } from '@mantine/hooks';
import { notifications } from '@mantine/notifications';
import { useQuery, useQueryClient } from '@tanstack/react-query';

import { api, errorMessage } from '@/shared/api/client';
import { type OwnerEntry, ownersQuery } from '@/shared/owners';
import { OwnerSelect } from '@/shared/ui/OwnerSelect';
import { QueryState } from '@/shared/ui/QueryState';

import { reloadWorkspaces } from './api';
import {
  type CreateValues,
  emptyCreateValues,
  fullName,
  toCreateBody,
  validateIdle,
  validateName,
} from './create';

function CreateForm({
  owners,
  onCreated,
  onCancel,
}: {
  owners: OwnerEntry[];
  onCreated: (name: string) => void;
  onCancel: () => void;
}) {
  const queryClient = useQueryClient();
  const form = useForm<CreateValues>({
    initialValues: emptyCreateValues(owners[0]?.owner ?? ''),
    validate: {
      name: validateName,
      idleAfter: (_, values) => validateIdle(values),
    },
  });
  const owner = owners.find((o) => o.owner === form.values.owner);
  const prefix = owner?.prefix ?? '';
  const create = api.useMutation('post', '/api/v1/workspaces', {
    onError: (error) =>
      notifications.show({ color: 'red', title: 'Could not create', message: errorMessage(error) }),
    onSettled: () => reloadWorkspaces(queryClient),
  });

  const submit = form.onSubmit((values) =>
    create.mutate(
      { body: toCreateBody(prefix, values) },
      {
        onSuccess: (created) => {
          notifications.show({
            color: 'teal',
            title: `Created ${created.name}`,
            message: 'It starts when you open a terminal.',
          });
          onCreated(created.name);
        },
      },
    ),
  );

  return (
    <form onSubmit={submit}>
      <Stack>
        <OwnerSelect
          label="Owner"
          description="Who the workspace belongs to. Members of an organization or team can all use it."
          owners={owners}
          {...form.getInputProps('owner')}
        />
        <TextInput
          label="Name"
          data-autofocus
          autoComplete="off"
          description={
            <>
              SSH name:{' '}
              <Text span ff="monospace" size="xs">
                {fullName(prefix, form.values.name || 'name')}
              </Text>
            </>
          }
          {...form.getInputProps('name')}
        />
        <Stack gap={4}>
          <Text size="sm" fw={500}>
            Root
          </Text>
          <Text size="xs" c="dimmed">
            Ephemeral resets everything outside the workspace disk on each start. Default follows
            the owner&apos;s settings.
          </Text>
          <SegmentedControl
            data={[
              { value: 'default', label: 'Default' },
              { value: 'ephemeral', label: 'Ephemeral' },
              { value: 'persistent', label: 'Persistent' },
            ]}
            {...form.getInputProps('root')}
          />
        </Stack>
        <Accordion variant="contained">
          <Accordion.Item value="advanced">
            <Accordion.Control>Advanced</Accordion.Control>
            <Accordion.Panel>
              <Stack>
                <Text size="xs" c="dimmed">
                  Leave a field empty to use the gateway&apos;s default.
                </Text>
                <SimpleGrid cols={{ base: 1, xs: 2 }}>
                  <NumberInput
                    label="vCPUs"
                    min={1}
                    max={32}
                    allowDecimal={false}
                    {...form.getInputProps('vcpus')}
                  />
                  <NumberInput
                    label="Memory (GiB)"
                    min={0.5}
                    decimalScale={1}
                    {...form.getInputProps('memoryGib')}
                  />
                  <NumberInput
                    label="Workspace disk (GiB)"
                    min={1}
                    allowDecimal={false}
                    {...form.getInputProps('workspaceDiskGib')}
                  />
                  <NumberInput
                    label="Docker disk (GiB)"
                    min={1}
                    allowDecimal={false}
                    {...form.getInputProps('dockerDiskGib')}
                  />
                  <Select
                    label="Idle stop"
                    allowDeselect={false}
                    data={[
                      { value: 'default', label: "Gateway's default" },
                      { value: 'after', label: 'After a while' },
                      { value: 'never', label: 'Never' },
                    ]}
                    {...form.getInputProps('idle')}
                  />
                  {form.values.idle === 'after' && (
                    <TextInput
                      label="Idle time"
                      description="e.g. 30m, 2h or 1d"
                      {...form.getInputProps('idleAfter')}
                    />
                  )}
                </SimpleGrid>
              </Stack>
            </Accordion.Panel>
          </Accordion.Item>
        </Accordion>
        <Group justify="flex-end" gap="xs">
          <Button variant="default" onClick={onCancel} disabled={create.isPending}>
            Cancel
          </Button>
          <Button type="submit" loading={create.isPending}>
            Create
          </Button>
        </Group>
      </Stack>
    </form>
  );
}

// 新しいワークスペースを作るモーダル。作っただけでは VM は起動しない。
export function CreateWorkspace({
  opened,
  onClose,
  onCreated,
}: {
  opened: boolean;
  onClose: () => void;
  onCreated: (name: string) => void;
}) {
  const narrow = useMediaQuery('(max-width: 47.99em)') ?? true;
  const owners = useQuery({ ...ownersQuery(), enabled: opened });
  return (
    <Modal opened={opened} onClose={onClose} fullScreen={narrow} title="New workspace">
      {owners.data ? (
        <CreateForm owners={owners.data} onCreated={onCreated} onCancel={onClose} />
      ) : (
        <QueryState pending={owners.isPending} error={owners.error} empty={false} emptyText="" />
      )}
    </Modal>
  );
}
