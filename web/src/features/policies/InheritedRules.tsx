import { Card, Group, Stack, Text, UnstyledButton } from '@mantine/core';
import { useDisclosure } from '@mantine/hooks';
import { IconChevronDown, IconChevronRight } from '@tabler/icons-react';

import type { Schemas } from '@/shared/api/client';

import { RuleSummary } from './RuleSummary';
import { sourceLabel } from './rules';

// 上の層の規則 (`inherited` で選んだもの)。その層の規則より先に評価するので、覆せない。
export function InheritedRules({
  rules,
  inherited,
}: {
  rules: Schemas['EffectiveRule'][];
  inherited: (source: Schemas['RuleSource']) => boolean;
}) {
  const [opened, { toggle }] = useDisclosure(false);
  const shown = rules.filter((r) => inherited(r.source));
  if (shown.length === 0) return null;
  return (
    <Stack gap="xs">
      <UnstyledButton onClick={toggle} aria-expanded={opened}>
        <Group gap={4}>
          {opened ? <IconChevronDown size={14} /> : <IconChevronRight size={14} />}
          <Text size="sm" fw={500}>
            Rules from other layers ({shown.length})
          </Text>
        </Group>
      </UnstyledButton>
      <Text size="xs" c="dimmed">
        Checked before the rules below, in this order. The first rule that matches decides, so the
        rules below cannot override them.
      </Text>
      {opened && (
        <Stack gap="xs">
          {shown.map(({ rule, source }, i) => (
            <Card key={i} withBorder padding="xs">
              <RuleSummary rule={rule} source={sourceLabel(source)} />
            </Card>
          ))}
        </Stack>
      )}
    </Stack>
  );
}
