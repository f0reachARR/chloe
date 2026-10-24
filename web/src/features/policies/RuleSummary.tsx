import { Badge, Code, Group, Stack, Text } from '@mantine/core';

import type { Schemas } from '@/shared/api/client';

const actionColor: Record<Schemas['Action'], string> = {
  allow: 'teal',
  deny: 'red',
  passthrough: 'gray',
};

// 一つの規則を読むだけで出す。宛先とパスは利用者や VM の中の依頼が書くので、文字列のまま出す。
export function RuleSummary({
  rule,
  source,
}: {
  rule: Schemas['PathRuleConfig'];
  source?: string;
}) {
  return (
    <Stack gap={2}>
      <Group gap="xs" wrap="nowrap">
        <Badge size="sm" variant="light" color={actionColor[rule.action]}>
          {rule.action}
        </Badge>
        <Text size="sm" ff="monospace" style={{ wordBreak: 'break-all' }}>
          {rule.hosts.join(', ')}
        </Text>
        {source && (
          <Badge size="xs" variant="outline" color="gray" style={{ flexShrink: 0 }}>
            {source}
          </Badge>
        )}
      </Group>
      {(rule.methods?.length || rule.paths?.length || rule.name) && (
        <Group gap={4}>
          {rule.methods?.map((m) => (
            <Badge key={m} size="xs" variant="outline">
              {m}
            </Badge>
          ))}
          {rule.paths?.map((p) => (
            <Code key={p}>{p}</Code>
          ))}
          {rule.name && (
            <Text size="xs" c="dimmed">
              {rule.name}
            </Text>
          )}
        </Group>
      )}
    </Stack>
  );
}
