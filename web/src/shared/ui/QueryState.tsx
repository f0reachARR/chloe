import { Alert, Group, Loader, Text } from '@mantine/core';

import { errorMessage } from '@/shared/api/client';

// 一覧の読み込み中、誤り、空のときの表示。
export function QueryState({
  pending,
  error,
  empty,
  emptyText,
}: {
  pending: boolean;
  error: unknown;
  empty: boolean;
  emptyText: string;
}) {
  if (pending) {
    return (
      <Group justify="center" py="xl">
        <Loader />
      </Group>
    );
  }
  if (error) {
    return <Alert color="red">{errorMessage(error)}</Alert>;
  }
  if (empty) {
    return (
      <Text c="dimmed" ta="center" py="xl">
        {emptyText}
      </Text>
    );
  }
  return null;
}
