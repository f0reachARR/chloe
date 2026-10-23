import { Button, Group } from '@mantine/core';
import { IconRefresh } from '@tabler/icons-react';
import type { ReactNode } from 'react';

// 一覧の上の帯。左に絞り込みや説明、右に読み直すボタンを置く。
export function Toolbar({
  children,
  reloading,
  onReload,
}: {
  children?: ReactNode;
  reloading: boolean;
  onReload: () => void;
}) {
  return (
    <Group justify="space-between" wrap="nowrap">
      <div>{children}</div>
      <Button
        variant="subtle"
        size="compact-sm"
        leftSection={<IconRefresh size={14} />}
        loading={reloading}
        onClick={onReload}
      >
        Reload
      </Button>
    </Group>
  );
}
