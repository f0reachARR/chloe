import { Alert, Button, Group, Text } from '@mantine/core';

// 編集しているあいだに書き換えられていたとき (412) に、最新を読むか上書きするかを選ばせる。
export function StaleAlert({
  what,
  saving,
  onLoadLatest,
  onSaveMine,
}: {
  what: string;
  saving: boolean;
  onLoadLatest: () => void;
  onSaveMine: () => void;
}) {
  return (
    <Alert color="yellow" title={`The ${what} changed while you were editing`}>
      <Text size="sm" mb="xs">
        Someone saved these {what} (or approved an ask) after you opened them. Load the latest (your
        edits are lost), or save yours over them.
      </Text>
      <Group gap="xs">
        <Button size="compact-sm" variant="default" onClick={onLoadLatest}>
          Load latest
        </Button>
        <Button size="compact-sm" color="red" loading={saving} onClick={onSaveMine}>
          Save mine
        </Button>
      </Group>
    </Alert>
  );
}
