import {
  ActionIcon,
  Alert,
  Autocomplete,
  Badge,
  Button,
  Card,
  Code,
  Divider,
  Group,
  SegmentedControl,
  Select,
  Stack,
  TagsInput,
  Text,
  Title,
  Tooltip,
} from '@mantine/core';
import { type UseFormReturnType, useForm } from '@mantine/form';
import { IconArrowLeft, IconPlus, IconTrash } from '@tabler/icons-react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { Link } from '@tanstack/react-router';
import { useState } from 'react';

import { type Schemas, fetchClient } from '@/shared/api/client';
import { QueryState } from '@/shared/ui/QueryState';

import { policyRulesQuery, reloadPolicies } from './api';
import { InheritedRules } from './InheritedRules';
import {
  type LayerValues,
  emptyPermission,
  kindLabel,
  permissionLevels,
  permissionNames,
  sameLayer,
  toLayer,
  toLayerValues,
} from './layer';
import { RuleList, ruleValidation } from './RuleList';
import { RuleSummary } from './RuleSummary';
import { StaleAlert } from './StaleAlert';
import { checkStale, useVersionedSave } from './versioned';

type PolicyLayer = Schemas['PolicyLayer'];
type PolicyRules = Schemas['PolicyRules'];

// `user:alice`、`org:acme`、`team:acme/dev` を、種類と名前に分ける (`gateway` は持ち主ではない)。
export function parseScope(scope: string): { kind: Schemas['ScopeKind']; name: string } {
  if (scope === 'gateway') return { kind: 'gateway', name: 'gateway' };
  const [kind, name] = [scope.slice(0, scope.indexOf(':')), scope.slice(scope.indexOf(':') + 1)];
  return { kind: kind as Schemas['ScopeKind'], name };
}

const noRules = 'This layer adds no rules of its own.';

function GitHubFields({ form }: { form: UseFormReturnType<LayerValues> }) {
  const values = form.values;
  return (
    <Stack gap="xs">
      <Text size="sm" fw={500}>
        GitHub token scope
      </Text>
      <Text size="xs" c="dimmed">
        The repositories and permissions of the installation token given to shared workspaces. A
        lower layer&apos;s scope wins.
      </Text>
      <SegmentedControl
        aria-label="GitHub token scope"
        data={[
          { value: 'inherit', label: 'Not set' },
          { value: 'set', label: 'Limit' },
        ]}
        {...form.getInputProps('github')}
      />
      {values.github === 'set' && (
        <Stack gap="xs">
          <SegmentedControl
            aria-label="Repositories"
            size="xs"
            data={[
              { value: 'all', label: 'All repositories' },
              { value: 'listed', label: 'Only these' },
            ]}
            {...form.getInputProps('repositories')}
          />
          {values.repositories === 'listed' && (
            <TagsInput
              label="Repository names"
              placeholder="web, api"
              splitChars={[',', ' ']}
              {...form.getInputProps('repositoryList')}
            />
          )}
          <SegmentedControl
            aria-label="Permissions"
            size="xs"
            data={[
              { value: 'all', label: 'All permissions' },
              { value: 'listed', label: 'Only these' },
            ]}
            {...form.getInputProps('permissions')}
          />
          {values.permissions === 'listed' && (
            <Stack gap={6}>
              {values.permissionList.map((permission, index) => (
                <Group key={permission.key} gap="xs" wrap="nowrap" align="flex-start">
                  <Autocomplete
                    aria-label={`Permission ${index + 1}`}
                    placeholder="contents"
                    data={permissionNames}
                    flex={1}
                    {...form.getInputProps(`permissionList.${index}.name`)}
                  />
                  <Select
                    aria-label={`Level of permission ${index + 1}`}
                    data={permissionLevels}
                    allowDeselect={false}
                    w={110}
                    {...form.getInputProps(`permissionList.${index}.level`)}
                  />
                  <Tooltip label="Remove">
                    <ActionIcon
                      type="button"
                      variant="subtle"
                      color="red"
                      mt={4}
                      aria-label={`Remove permission ${index + 1}`}
                      onClick={() => form.removeListItem('permissionList', index)}
                    >
                      <IconTrash size={16} />
                    </ActionIcon>
                  </Tooltip>
                </Group>
              ))}
              <Group>
                <Button
                  size="compact-sm"
                  variant="light"
                  leftSection={<IconPlus size={14} />}
                  onClick={() => form.insertListItem('permissionList', emptyPermission())}
                >
                  Add permission
                </Button>
              </Group>
            </Stack>
          )}
        </Stack>
      )}
    </Stack>
  );
}

function LayerForm({
  scope,
  data,
}: {
  scope: { kind: Schemas['ScopeKind']; name: string; id: string };
  data: PolicyRules;
}) {
  const queryClient = useQueryClient();
  const personal = scope.kind === 'user';
  // 保存してある設定 (変更があるかを比べる)。
  const [saved, setSaved] = useState(data.own);
  const form = useForm<LayerValues>({
    initialValues: toLayerValues(data.own),
    validate: {
      rules: ruleValidation,
      repositoryList: (list, values) =>
        values.github === 'set' && values.repositories === 'listed' && list.length === 0
          ? 'Add at least one repository'
          : null,
      permissionList: {
        name: (name) => (name.trim() ? null : 'Write a permission name'),
      },
    },
  });
  const dirty = !sameLayer(toLayer(form.values, personal), saved);

  const versioned = useVersionedSave<PolicyRules, PolicyLayer>({
    data,
    dirty,
    reset: (latest) => {
      const values = toLayerValues(latest.own);
      setSaved(latest.own);
      form.setInitialValues(values);
      form.setValues(values);
    },
    put: async (version, layer) =>
      checkStale(
        personal
          ? await fetchClient.PUT('/api/v1/policy', {
              params: { header: { 'If-Match': version } },
              body: layer,
            })
          : await fetchClient.PUT('/api/v1/policies/{owner}', {
              params: { path: { owner: scope.name }, header: { 'If-Match': version } },
              body: layer,
            }),
      ),
    refetch: async () => {
      void reloadPolicies(queryClient);
      return queryClient.fetchQuery({ ...policyRulesQuery(scope.id), staleTime: 0 });
    },
    onSent: setSaved,
    savedMessage: `Saved the settings of ${scope.name}`,
  });

  return (
    <form onSubmit={form.onSubmit((values) => versioned.save(toLayer(values, personal)))}>
      <Stack>
        {versioned.stale && (
          <StaleAlert
            what="settings"
            saving={versioned.saving}
            onLoadLatest={versioned.loadLatest}
            onSaveMine={() => versioned.saveOverLatest(toLayer(form.values, personal))}
          />
        )}
        <Text size="sm" fw={500}>
          Rules
        </Text>
        <RuleList form={form} emptyText={noRules} />
        <Divider />
        <Stack gap={4}>
          <Text size="sm" fw={500}>
            Default root for new workspaces
          </Text>
          <Text size="xs" c="dimmed">
            Not set follows the layer above, or the gateway&apos;s default.
          </Text>
          <SegmentedControl
            aria-label="Default root"
            data={[
              { value: 'inherit', label: 'Not set' },
              { value: 'ephemeral', label: 'Ephemeral' },
              { value: 'persistent', label: 'Persistent' },
            ]}
            {...form.getInputProps('root')}
          />
        </Stack>
        {!personal && (
          <>
            <Divider />
            <GitHubFields form={form} />
          </>
        )}
        <Group justify="flex-end" gap="xs">
          <Button
            variant="default"
            disabled={!dirty || versioned.saving}
            onClick={versioned.loadLatest}
          >
            Reset
          </Button>
          <Button type="submit" disabled={!dirty} loading={versioned.saving}>
            Save
          </Button>
        </Group>
      </Stack>
    </form>
  );
}

// 変えられない人に、層の設定を読むだけで出す。
function LayerSummary({ layer, personal }: { layer: PolicyLayer; personal: boolean }) {
  return (
    <Stack gap="xs">
      {(layer.rules ?? []).length === 0 ? (
        <Text size="sm" c="dimmed">
          {noRules}
        </Text>
      ) : (
        layer.rules?.map((rule, i) => (
          <Card key={i} withBorder padding="xs">
            <RuleSummary rule={rule} />
          </Card>
        ))
      )}
      <Text size="sm">
        Default root: <Code>{layer.root ?? 'not set'}</Code>
      </Text>
      {!personal && (
        <Text size="sm">
          GitHub token scope:{' '}
          {layer.github ? (
            <Code>
              {layer.github.repositories?.join(', ') ?? 'all repositories'};{' '}
              {layer.github.permissions
                ? Object.entries(layer.github.permissions)
                    .map(([name, level]) => `${name}=${level}`)
                    .join(', ')
                : 'all permissions'}
            </Code>
          ) : (
            <Code>not set</Code>
          )}
        </Text>
      )}
    </Stack>
  );
}

// 一つの層 (ゲートウェイか持ち主) の設定。上の層の規則を読むだけで出し、その層の設定を変える。
export function ScopePage({ scope }: { scope: string }) {
  const { kind, name } = parseScope(scope);
  const rules = useQuery(policyRulesQuery(scope));
  const ownSource = (source: Schemas['RuleSource']) =>
    source.kind === 'owner' && source.owner === name;
  return (
    <Stack>
      <Group gap="xs" wrap="nowrap">
        <Button
          component={Link}
          to="/policies"
          variant="subtle"
          size="compact-sm"
          px={4}
          aria-label="Back to policies"
        >
          <IconArrowLeft size={16} />
        </Button>
        <Title order={4} ff="monospace" style={{ overflowWrap: 'anywhere' }}>
          {name}
        </Title>
        <Badge size="sm" variant="light">
          {kindLabel[kind]}
        </Badge>
      </Group>
      {!rules.data ? (
        <QueryState pending={rules.isPending} error={rules.error} empty={false} emptyText="" />
      ) : kind === 'gateway' ? (
        <Stack gap="xs">
          <Text size="sm" c="dimmed">
            The rules every workspace starts from. They are set in the gateway&apos;s configuration
            file and checked before all other layers.
          </Text>
          {rules.data.rules.map(({ rule }, i) => (
            <Card key={i} withBorder padding="xs">
              <RuleSummary rule={rule} />
            </Card>
          ))}
        </Stack>
      ) : (
        <Stack>
          <InheritedRules
            rules={rules.data.rules}
            inherited={(source) => source.kind !== 'secret' && !ownSource(source)}
          />
          {rules.data.editable ? (
            <LayerForm scope={{ kind, name, id: scope }} data={rules.data} />
          ) : (
            <Stack gap="xs">
              <Alert color="gray">
                You can view but not change these settings. Only organization admins (or the
                team&apos;s maintainers) can, and personal settings may be turned off on this
                gateway.
              </Alert>
              <LayerSummary layer={rules.data.own} personal={kind === 'user'} />
            </Stack>
          )}
        </Stack>
      )}
    </Stack>
  );
}
