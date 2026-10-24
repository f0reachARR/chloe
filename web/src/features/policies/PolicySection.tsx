import { Alert, Button, Card, Group, Stack, Text } from '@mantine/core';
import { useForm } from '@mantine/form';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useEffect, useState } from 'react';

import { type Schemas, fetchClient } from '@/shared/api/client';
import type { WorkspaceSettingsSectionProps } from '@/shared/feature';
import { QueryState } from '@/shared/ui/QueryState';

import { rulesQuery } from './api';
import { InheritedRules } from './InheritedRules';
import { RuleList, ruleValidation } from './RuleList';
import { RuleSummary } from './RuleSummary';
import { type PolicyValues, sameRules, toRuleConfigs, toRuleValues } from './rules';
import { StaleAlert } from './StaleAlert';
import { checkStale, useVersionedSave } from './versioned';

type WorkspaceRules = Schemas['WorkspaceRules'];
type PathRuleConfig = Schemas['PathRuleConfig'];

const noRules = 'This workspace adds no rules of its own.';

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
  // 保存してある規則 (変更があるかを比べる)。
  const [saved, setSaved] = useState(data.own);
  const form = useForm<PolicyValues>({
    initialValues: { rules: data.own.map(toRuleValues) },
    validate: { rules: ruleValidation },
  });
  const dirty = !sameRules(toRuleConfigs(form.values), saved);
  useEffect(() => onDirtyChange(dirty), [dirty, onDirtyChange]);

  const versioned = useVersionedSave<WorkspaceRules, PathRuleConfig[]>({
    data,
    dirty,
    reset: (latest) => {
      const values = { rules: latest.own.map(toRuleValues) };
      setSaved(latest.own);
      form.setInitialValues(values);
      form.setValues(values);
    },
    put: async (version, rules) =>
      checkStale(
        await fetchClient.PUT('/api/v1/workspace-policies/{name}', {
          params: { path: { name: workspace }, header: { 'If-Match': version } },
          body: { rules },
        }),
      ),
    refetch: () => queryClient.fetchQuery({ ...rulesQuery(workspace), staleTime: 0 }),
    onSent: setSaved,
    savedMessage: `Saved the rules of ${workspace}`,
  });

  return (
    <form onSubmit={form.onSubmit((values) => versioned.save(toRuleConfigs(values)))}>
      <Stack gap="sm">
        {versioned.stale && (
          <StaleAlert
            what="rules"
            saving={versioned.saving}
            onLoadLatest={versioned.loadLatest}
            onSaveMine={() => versioned.saveOverLatest(toRuleConfigs(form.values))}
          />
        )}
        <RuleList form={form} emptyText={noRules} />
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
      <InheritedRules
        rules={rules.data.rules}
        inherited={(source) => source.kind !== 'workspace'}
      />
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
              {noRules}
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
