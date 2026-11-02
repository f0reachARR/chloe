import {
  Alert,
  Badge,
  Button,
  Card,
  Code,
  Group,
  Menu,
  Modal,
  PasswordInput,
  Stack,
  Text,
} from '@mantine/core';
import { useForm } from '@mantine/form';
import { useMediaQuery } from '@mantine/hooks';
import { notifications } from '@mantine/notifications';
import { IconChevronDown, IconKey, IconPencil, IconPlus, IconTrash } from '@tabler/icons-react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useState } from 'react';

import { type Schemas, api, errorMessage } from '@/shared/api/client';
import { type OwnerEntry, ownersQuery } from '@/shared/owners';
import { OwnerSelect } from '@/shared/ui/OwnerSelect';
import { QueryState } from '@/shared/ui/QueryState';
import { Toolbar } from '@/shared/ui/Toolbar';

import { ownerQuery, reloadSecrets, secretsQuery, templatesQuery } from './api';
import {
  type SecretValues,
  emptySecret,
  fromSpec,
  secretEnv,
  secretHosts,
  toSecretInput,
  toValueInput,
} from './secret';
import { SecretForm } from './SecretForm';

type SecretEntry = Schemas['SecretView'];

// 組み込みの種類 (定義を登録しなくても値だけで使える)。
const BUILTIN = 'github_token';

type Editing =
  | { mode: 'new'; initial: SecretValues }
  | { mode: 'definition'; entry: SecretEntry }
  | { mode: 'value'; entry: SecretEntry };

// 登録の mutation。送った中身 (値) を状態に残さないよう、終わったら捨てる。
function useSaveSecret(owner: OwnerEntry, onSaved: () => void) {
  const queryClient = useQueryClient();
  const save = api.useMutation('put', '/api/v1/secrets/{name}', {
    gcTime: 0,
    onSettled: () => reloadSecrets(queryClient),
  });
  const mutate = (name: string, body: Schemas['SecretInput'], clear: () => void) =>
    save.mutate(
      { params: { path: { name }, query: ownerQuery(owner) }, body },
      {
        onSuccess: () => {
          notifications.show({
            color: 'teal',
            title: `Saved ${name} for ${owner.name}`,
            message:
              'Running workspaces use it now. New environment variables reach shells opened after this.',
          });
          onSaved();
        },
        onError: (error) =>
          notifications.show({
            color: 'red',
            title: 'Could not save',
            message: errorMessage(error),
          }),
        // 成功しても失敗しても、値の欄を空にし、送った中身を捨てる。捨てるのは呼び出しごとの
        // 処理の最後にする (先に捨てると、TanStack Query は呼び出しごとの処理を呼ばない)。
        onSettled: () => {
          clear();
          save.reset();
        },
      },
    );
  return { mutate, saving: save.isPending };
}

function ValueForm({
  entry,
  saving,
  onSubmit,
  onCancel,
}: {
  entry: SecretEntry;
  saving: boolean;
  onSubmit: (values: Record<string, string>, clear: () => void) => void;
  onCancel: () => void;
}) {
  const names = Object.keys(entry.spec.fields);
  const empty = Object.fromEntries(names.map((name) => [name, '']));
  const form = useForm<Record<string, string>>({
    initialValues: empty,
    validate: Object.fromEntries(
      names.map((name) => [name, (value: string) => (value === '' ? 'Enter the value' : null)]),
    ),
  });
  return (
    <form onSubmit={form.onSubmit((values) => onSubmit(values, () => form.setValues(empty)))}>
      <Stack>
        <Text size="sm" c="dimmed">
          The definition stays the same. Values cannot be read back, only replaced.
        </Text>
        {names.map((name, index) => (
          <PasswordInput
            key={name}
            label={names.length === 1 ? 'Value' : name}
            autoComplete="new-password"
            data-autofocus={index === 0 || undefined}
            {...form.getInputProps(name)}
          />
        ))}
        <Group justify="flex-end" gap="xs">
          <Button variant="default" onClick={onCancel} disabled={saving}>
            Cancel
          </Button>
          <Button type="submit" loading={saving}>
            Save
          </Button>
        </Group>
      </Stack>
    </form>
  );
}

function SecretCard({
  entry,
  from,
  manage,
  onEdit,
  owner,
}: {
  entry: SecretEntry;
  // Organization から受け継いだもの (team の画面で、読むだけで出す)。
  from?: string;
  manage: boolean;
  onEdit: (editing: Editing) => void;
  owner: OwnerEntry;
}) {
  const queryClient = useQueryClient();
  const [confirming, setConfirming] = useState(false);
  const remove = api.useMutation('delete', '/api/v1/secrets/{name}', {
    onSuccess: () => notifications.show({ color: 'teal', message: `Removed ${entry.name}` }),
    onError: (error) =>
      notifications.show({ color: 'red', title: 'Could not remove', message: errorMessage(error) }),
    onSettled: () => reloadSecrets(queryClient),
  });
  const env = secretEnv(entry.spec);
  const hosts = secretHosts(entry.spec);
  const files = (entry.spec.files ?? []).map((file) => file.path);
  return (
    <Card withBorder padding="md" aria-label={`Secret ${entry.name}`}>
      <Stack gap="xs">
        <Group justify="space-between" wrap="nowrap">
          <Group gap="xs">
            <Text fw={600} ff="monospace">
              {entry.name}
            </Text>
            {entry.builtin && (
              <Badge size="sm" variant="light" color="gray">
                Built-in
              </Badge>
            )}
            {from && (
              <Badge size="sm" variant="light">
                from {from}
              </Badge>
            )}
          </Group>
          {manage && !from && (
            <Menu position="bottom-end">
              <Menu.Target>
                <Button
                  size="compact-sm"
                  variant="default"
                  rightSection={<IconChevronDown size={14} />}
                  aria-label={`Change ${entry.name}`}
                >
                  Change
                </Button>
              </Menu.Target>
              <Menu.Dropdown>
                <Menu.Item
                  leftSection={<IconKey size={14} />}
                  onClick={() => onEdit({ mode: 'value', entry })}
                >
                  Update value
                </Menu.Item>
                {!entry.builtin && (
                  <Menu.Item
                    leftSection={<IconPencil size={14} />}
                    onClick={() => onEdit({ mode: 'definition', entry })}
                  >
                    Edit definition
                  </Menu.Item>
                )}
                <Menu.Item
                  color="red"
                  leftSection={<IconTrash size={14} />}
                  onClick={() => setConfirming(true)}
                >
                  Remove
                </Menu.Item>
              </Menu.Dropdown>
            </Menu>
          )}
        </Group>
        {env.length > 0 && (
          <Group gap={4}>
            <Text size="xs" c="dimmed">
              Env:
            </Text>
            {env.map((name) => (
              <Code key={name}>{name}</Code>
            ))}
          </Group>
        )}
        {hosts.length > 0 && (
          <Text size="xs" c="dimmed">
            Sent to {hosts.join(', ')}
          </Text>
        )}
        {files.length > 0 && (
          <Text size="xs" c="dimmed">
            Files: {files.join(', ')}
          </Text>
        )}
        {confirming && (
          <Alert color="red" title={`Remove ${entry.name} from ${owner.name}?`}>
            <Text size="sm" mb="xs">
              Running workspaces stop getting the real value. Requests with the placeholder are no
              longer rewritten.
            </Text>
            <Group gap="xs">
              <Button
                size="compact-sm"
                color="red"
                loading={remove.isPending}
                onClick={() =>
                  remove.mutate(
                    { params: { path: { name: entry.name }, query: ownerQuery(owner) } },
                    { onSettled: () => setConfirming(false) },
                  )
                }
              >
                Remove
              </Button>
              <Button size="compact-sm" variant="default" onClick={() => setConfirming(false)}>
                Cancel
              </Button>
            </Group>
          </Alert>
        )}
      </Stack>
    </Card>
  );
}

function NewSecretMenu({ onEdit }: { onEdit: (editing: Editing) => void }) {
  const templates = useQuery(templatesQuery());
  const builtin = (): SecretEntry => ({
    name: BUILTIN,
    owner: '',
    builtin: true,
    spec: { fields: { value: { placeholder: '' } } },
  });
  return (
    <Menu position="bottom-start">
      <Menu.Target>
        <Button
          size="compact-sm"
          leftSection={<IconPlus size={14} />}
          rightSection={<IconChevronDown size={14} />}
        >
          New secret
        </Button>
      </Menu.Target>
      <Menu.Dropdown>
        <Menu.Item onClick={() => onEdit({ mode: 'new', initial: emptySecret() })}>Blank</Menu.Item>
        <Menu.Item onClick={() => onEdit({ mode: 'value', entry: builtin() })}>
          {BUILTIN} (built-in)
        </Menu.Item>
        {(templates.data ?? []).length > 0 && <Menu.Label>Templates</Menu.Label>}
        {templates.data?.map((template) => (
          <Menu.Item
            key={template.name}
            onClick={() => onEdit({ mode: 'new', initial: fromSpec(template.name, template.spec) })}
          >
            {template.name}
          </Menu.Item>
        ))}
      </Menu.Dropdown>
    </Menu>
  );
}

function OwnerSecrets({ owner }: { owner: OwnerEntry }) {
  const narrow = useMediaQuery('(max-width: 47.99em)') ?? true;
  const secrets = useQuery(secretsQuery(owner));
  // team のワークスペースは、team にない名前なら Organization のシークレットを使う。
  const org = owner.kind === 'team' ? owner.name.split('/')[0] : undefined;
  const orgSecrets = useQuery({
    ...secretsQuery({ kind: 'org', name: org ?? '' }),
    enabled: org != null,
  });
  const [editing, setEditing] = useState<Editing>();
  const close = () => setEditing(undefined);
  const save = useSaveSecret(owner, close);
  const manage = owner.can_manage_secrets;
  const own = secrets.data?.entries ?? [];
  const inherited = (orgSecrets.data?.entries ?? []).filter(
    (entry) => !own.some((mine) => mine.name === entry.name),
  );

  const title =
    editing?.mode === 'new'
      ? 'New secret'
      : editing?.mode === 'value'
        ? `Update ${editing.entry.name}`
        : editing
          ? `Edit ${editing.entry.name}`
          : '';

  return (
    <Stack>
      <Toolbar reloading={secrets.isFetching} onReload={() => secrets.refetch()}>
        {manage ? (
          <NewSecretMenu onEdit={setEditing} />
        ) : (
          <Text size="xs" c="dimmed">
            Only{' '}
            {owner.kind === 'team'
              ? 'organization admins and team maintainers'
              : 'organization admins'}{' '}
            can change these.
          </Text>
        )}
      </Toolbar>
      <QueryState
        pending={secrets.isPending}
        error={secrets.error}
        empty={own.length === 0 && inherited.length === 0}
        emptyText="No secrets yet."
      />
      {own.map((entry) => (
        <SecretCard
          key={entry.name}
          entry={entry}
          manage={manage}
          onEdit={setEditing}
          owner={owner}
        />
      ))}
      {inherited.map((entry) => (
        <SecretCard
          key={entry.name}
          entry={entry}
          from={org}
          manage={false}
          onEdit={setEditing}
          owner={owner}
        />
      ))}
      <Modal opened={editing != null} onClose={close} fullScreen={narrow} size="lg" title={title}>
        {editing?.mode === 'value' && (
          <ValueForm
            entry={editing.entry}
            saving={save.saving}
            onCancel={close}
            onSubmit={(values, clear) =>
              save.mutate(
                editing.entry.name,
                toValueInput(editing.entry.name, editing.entry.spec, editing.entry.builtin, values),
                clear,
              )
            }
          />
        )}
        {(editing?.mode === 'new' || editing?.mode === 'definition') && (
          <SecretForm
            initial={
              editing.mode === 'new'
                ? editing.initial
                : fromSpec(editing.entry.name, editing.entry.spec)
            }
            editing={editing.mode === 'definition'}
            saving={save.saving}
            onCancel={close}
            onSubmit={(values, form) =>
              save.mutate(values.name, toSecretInput(values), () =>
                values.fields.forEach((_, index) =>
                  form.setFieldValue(`fields.${index}.value`, ''),
                ),
              )
            }
          />
        )}
      </Modal>
    </Stack>
  );
}

// 持ち主ごとのシークレット。持ち主は URL の `owner=` (`org:acme` など) に置く。
export function SecretsPage({
  owner,
  onOwnerChange,
}: {
  owner?: string;
  onOwnerChange: (owner: string) => void;
}) {
  const owners = useQuery(ownersQuery());
  if (!owners.data) {
    return (
      <QueryState pending={owners.isPending} error={owners.error} empty={false} emptyText="" />
    );
  }
  const selected = owners.data.find((o) => o.owner === owner) ?? owners.data[0];
  return (
    <Stack>
      <OwnerSelect
        label="Owner"
        owners={owners.data}
        value={selected.owner}
        onChange={(value) => value && onOwnerChange(value)}
      />
      <OwnerSecrets key={selected.owner} owner={selected} />
    </Stack>
  );
}
