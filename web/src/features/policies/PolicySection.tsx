import {
  ActionIcon,
  Alert,
  Button,
  Card,
  Group,
  SegmentedControl,
  Stack,
  TagsInput,
  Text,
  TextInput,
  Tooltip,
  UnstyledButton,
} from '@mantine/core';
import { useForm } from '@mantine/form';
import { useDisclosure } from '@mantine/hooks';
import { notifications } from '@mantine/notifications';
import {
  IconArrowDown,
  IconArrowUp,
  IconChevronDown,
  IconChevronRight,
  IconPlus,
  IconTrash,
} from '@tabler/icons-react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useEffect, useState } from 'react';

import { type Schemas, errorMessage, fetchClient } from '@/shared/api/client';
import type { WorkspaceSettingsSectionProps } from '@/shared/feature';
import { QueryState } from '@/shared/ui/QueryState';

import { reloadRules, rulesQuery } from './api';
import { RuleSummary } from './RuleSummary';
import {
  type PolicyValues,
  emptyRule,
  methodOptions,
  sameRules,
  sourceLabel,
  toRuleConfigs,
  toRuleValues,
} from './rules';

type WorkspaceRules = Schemas['WorkspaceRules'];

// 版 (If-Match) が違って断られた。読んだあとに、承認などで規則が書き換えられていた。
class StaleError extends Error {}

// 上の層の規則。ワークスペースの規則より先に評価するので、ワークスペースの規則では覆せない。
function InheritedRules({ rules }: { rules: WorkspaceRules['rules'] }) {
  const [opened, { toggle }] = useDisclosure(false);
  const inherited = rules.filter((r) => r.source.kind !== 'workspace');
  if (inherited.length === 0) return null;
  return (
    <Stack gap="xs">
      <UnstyledButton onClick={toggle} aria-expanded={opened}>
        <Group gap={4}>
          {opened ? <IconChevronDown size={14} /> : <IconChevronRight size={14} />}
          <Text size="sm" fw={500}>
            Rules from other layers ({inherited.length})
          </Text>
        </Group>
      </UnstyledButton>
      <Text size="xs" c="dimmed">
        Checked before this workspace&apos;s rules, in this order. The first rule that matches
        decides, so the rules below cannot override them.
      </Text>
      {opened && (
        <Stack gap="xs">
          {inherited.map(({ rule, source }, i) => (
            <Card key={i} withBorder padding="xs">
              <RuleSummary rule={rule} source={sourceLabel(source)} />
            </Card>
          ))}
        </Stack>
      )}
    </Stack>
  );
}

function PolicyForm({
  workspace,
  data,
  onDirtyChange,
}: {
  workspace: string;
  data: WorkspaceRules;
  onDirtyChange: (dirty: boolean) => void;
}) {
  const queryClient = useQueryClient();
  // フォームを作ったときの規則と版。読み直しで変わっても、保存するまでは変えない (If-Match に使う)。
  const [base, setBase] = useState({ own: data.own, version: data.version });
  const [stale, setStale] = useState(false);
  const form = useForm<PolicyValues>({
    initialValues: { rules: data.own.map(toRuleValues) },
    validate: {
      rules: {
        hosts: (hosts) => (hosts.length > 0 ? null : 'Add at least one host'),
      },
    },
  });
  const dirty = !sameRules(toRuleConfigs(form.values), base.own);
  useEffect(() => onDirtyChange(dirty), [dirty, onDirtyChange]);

  const reset = (latest: WorkspaceRules) => {
    const values = { rules: latest.own.map(toRuleValues) };
    setBase({ own: latest.own, version: latest.version });
    form.setInitialValues(values);
    form.setValues(values);
    setStale(false);
  };
  // 読み直して版が変わったとき、変えていなければ、その規則 (承認で足された規則など) に合わせる。
  // 変えていれば合わせず、保存したときに版の違いで断られる。
  const [seenVersion, setSeenVersion] = useState(data.version);
  if (data.version !== seenVersion) {
    setSeenVersion(data.version);
    if (!dirty && data.version !== base.version) reset(data);
  }

  const save = useMutation({
    mutationFn: async ({ version }: { version: string }) => {
      const {
        data: updated,
        error,
        response,
      } = await fetchClient.PUT('/api/v1/workspace-policies/{name}', {
        params: { path: { name: workspace }, header: { 'If-Match': version } },
        body: { rules: toRuleConfigs(form.values) },
      });
      if (response.status === 412) throw new StaleError(errorMessage(error));
      if (error) throw error;
      return updated;
    },
    onSuccess: async () => {
      notifications.show({ color: 'teal', message: `Saved the rules of ${workspace}` });
      reset(await queryClient.fetchQuery({ ...rulesQuery(workspace), staleTime: 0 }));
    },
    onError: (error) => {
      if (error instanceof StaleError) {
        setStale(true);
        void reloadRules(queryClient);
        return;
      }
      notifications.show({ color: 'red', title: 'Could not save', message: errorMessage(error) });
    },
  });

  const submit = form.onSubmit(() => save.mutate({ version: base.version }));
  const rules = form.values.rules;

  return (
    <form onSubmit={submit}>
      <Stack gap="sm">
        {stale && (
          <Alert color="yellow" title="The rules changed while you were editing">
            <Text size="sm" mb="xs">
              Someone saved these rules or approved an ask after you opened them. Load the latest
              rules (your edits are lost), or save yours over them.
            </Text>
            <Group gap="xs">
              <Button size="compact-sm" variant="default" onClick={() => reset(data)}>
                Load latest
              </Button>
              <Button
                size="compact-sm"
                color="red"
                loading={save.isPending}
                onClick={() => save.mutate({ version: data.version })}
              >
                Save mine
              </Button>
            </Group>
          </Alert>
        )}
        {rules.length === 0 && (
          <Text size="sm" c="dimmed">
            This workspace adds no rules of its own.
          </Text>
        )}
        {rules.map((rule, index) => (
          <Card key={rule.key} withBorder padding="sm">
            <Stack gap="xs">
              <Group justify="space-between" wrap="nowrap">
                <SegmentedControl
                  size="xs"
                  data={[
                    { value: 'allow', label: 'Allow' },
                    { value: 'deny', label: 'Deny' },
                  ]}
                  aria-label={`Action of rule ${index + 1}`}
                  {...form.getInputProps(`rules.${index}.action`)}
                />
                <Group gap={2} wrap="nowrap">
                  <Tooltip label="Move up">
                    <ActionIcon
                      type="button"
                      variant="subtle"
                      color="gray"
                      aria-label={`Move rule ${index + 1} up`}
                      disabled={index === 0}
                      onClick={() => form.reorderListItem('rules', { from: index, to: index - 1 })}
                    >
                      <IconArrowUp size={16} />
                    </ActionIcon>
                  </Tooltip>
                  <Tooltip label="Move down">
                    <ActionIcon
                      type="button"
                      variant="subtle"
                      color="gray"
                      aria-label={`Move rule ${index + 1} down`}
                      disabled={index === rules.length - 1}
                      onClick={() => form.reorderListItem('rules', { from: index, to: index + 1 })}
                    >
                      <IconArrowDown size={16} />
                    </ActionIcon>
                  </Tooltip>
                  <Tooltip label="Remove">
                    <ActionIcon
                      type="button"
                      variant="subtle"
                      color="red"
                      aria-label={`Remove rule ${index + 1}`}
                      onClick={() => form.removeListItem('rules', index)}
                    >
                      <IconTrash size={16} />
                    </ActionIcon>
                  </Tooltip>
                </Group>
              </Group>
              <TagsInput
                label={`Hosts of rule ${index + 1}`}
                placeholder="api.example.com, *.example.com"
                splitChars={[',', ' ']}
                {...form.getInputProps(`rules.${index}.hosts`)}
              />
              <Group grow align="flex-start">
                <TagsInput
                  label="Methods"
                  placeholder="All"
                  data={methodOptions}
                  {...form.getInputProps(`rules.${index}.methods`)}
                />
                <TagsInput
                  label="Paths"
                  placeholder="All, or /api/**"
                  splitChars={[',', ' ']}
                  {...form.getInputProps(`rules.${index}.paths`)}
                />
              </Group>
              <TextInput
                label="Name"
                placeholder="Shown in the request log"
                {...form.getInputProps(`rules.${index}.name`)}
              />
            </Stack>
          </Card>
        ))}
        <Group justify="space-between">
          <Button
            variant="light"
            leftSection={<IconPlus size={14} />}
            onClick={() => form.insertListItem('rules', emptyRule())}
          >
            Add rule
          </Button>
          <Group gap="xs">
            <Button
              variant="default"
              disabled={!dirty || save.isPending}
              onClick={() => reset(data)}
            >
              Reset
            </Button>
            <Button type="submit" disabled={!dirty} loading={save.isPending}>
              Save
            </Button>
          </Group>
        </Group>
      </Stack>
    </form>
  );
}

// ワークスペースの規則。上の層の規則を読むだけで出し、その下にワークスペース自身の規則を並べて変える。
// 規則の確かめはサーバーに任せ、保存したときの誤りを出す。
export function PolicySection({ workspace, onDirtyChange }: WorkspaceSettingsSectionProps) {
  const rules = useQuery(rulesQuery(workspace));
  if (!rules.data) {
    return <QueryState pending={rules.isPending} error={rules.error} empty={false} emptyText="" />;
  }
  return (
    <Stack>
      <Text size="sm" c="dimmed">
        Which hosts the agents in this workspace can reach. Rules are checked from the top; the
        first match decides, and anything that matches no rule is blocked.
      </Text>
      <InheritedRules rules={rules.data.rules} />
      <Text size="sm" fw={500}>
        This workspace&apos;s rules
      </Text>
      {rules.data.editable ? (
        <PolicyForm workspace={workspace} data={rules.data} onDirtyChange={onDirtyChange} />
      ) : (
        <Stack gap="xs">
          <Alert color="gray">
            You can view but not change these rules. Only organization admins (or the team&apos;s
            maintainers) can, and personal rules may be turned off on this gateway.
          </Alert>
          {rules.data.own.length === 0 ? (
            <Text size="sm" c="dimmed">
              This workspace adds no rules of its own.
            </Text>
          ) : (
            rules.data.own.map((rule, i) => (
              <Card key={i} withBorder padding="xs">
                <RuleSummary rule={rule} />
              </Card>
            ))
          )}
        </Stack>
      )}
    </Stack>
  );
}
