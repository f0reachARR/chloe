import {
  ActionIcon,
  Button,
  Card,
  Group,
  SegmentedControl,
  Stack,
  TagsInput,
  Text,
  TextInput,
  Tooltip,
} from '@mantine/core';
import type { UseFormReturnType } from '@mantine/form';
import { IconArrowDown, IconArrowUp, IconPlus, IconTrash } from '@tabler/icons-react';

import { type RuleValues, emptyRule, methodOptions } from './rules';

// 規則の欄の確かめ (`useForm` の `validate.rules` に渡す)。
export const ruleValidation = {
  hosts: (hosts: string[]) => (hosts.length > 0 ? null : 'Add at least one host'),
};

// 規則の並び。足す、消す、上下に動かす (最初に一致した規則で決まるので、並び順に意味がある)。
// ワークスペースの規則と、個人、Organization、team の設定で使う。
export function RuleList<T extends { rules: RuleValues[] }>({
  form,
  emptyText,
}: {
  form: UseFormReturnType<T>;
  emptyText: string;
}) {
  // `rules.0.hosts` のような欄の名前は、フォームの値の型からは作れないので、ここで受け流す。
  const field = (index: number, name: keyof RuleValues) =>
    form.getInputProps(`rules.${index}.${name}` as never);
  const rules = form.values.rules;
  return (
    <Stack gap="sm">
      {rules.length === 0 && (
        <Text size="sm" c="dimmed">
          {emptyText}
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
                {...field(index, 'action')}
              />
              {/* フォームの中のボタンは type="button" にする (欄で Enter を押したときに押されないように)。 */}
              <Group gap={2} wrap="nowrap">
                <Tooltip label="Move up">
                  <ActionIcon
                    type="button"
                    variant="subtle"
                    color="gray"
                    aria-label={`Move rule ${index + 1} up`}
                    disabled={index === 0}
                    onClick={() =>
                      form.reorderListItem('rules' as never, { from: index, to: index - 1 })
                    }
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
                    onClick={() =>
                      form.reorderListItem('rules' as never, { from: index, to: index + 1 })
                    }
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
                    onClick={() => form.removeListItem('rules' as never, index)}
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
              {...field(index, 'hosts')}
            />
            <Group grow align="flex-start">
              <TagsInput
                label="Methods"
                placeholder="All"
                data={methodOptions}
                {...field(index, 'methods')}
              />
              <TagsInput
                label="Paths"
                placeholder="All, or /api/**"
                splitChars={[',', ' ']}
                {...field(index, 'paths')}
              />
            </Group>
            <TextInput
              label="Name"
              placeholder="Shown in the request log"
              {...field(index, 'name')}
            />
          </Stack>
        </Card>
      ))}
      <Group>
        <Button
          variant="light"
          leftSection={<IconPlus size={14} />}
          onClick={() => form.insertListItem('rules' as never, emptyRule() as never)}
        >
          Add rule
        </Button>
      </Group>
    </Stack>
  );
}
