import { Anchor, Drawer, Stack, Text } from '@mantine/core';
import { useMediaQuery } from '@mantine/hooks';
import { useQuery } from '@tanstack/react-query';
import { Link } from '@tanstack/react-router';

import type { WorkspaceOverlayProps } from '@/shared/feature';
import { QueryState } from '@/shared/ui/QueryState';

import { asksQuery } from './api';
import { AskCard } from './AskCard';

// 一つのワークスペースの、答えを待つ依頼。端末などの上に重ねて開き、下の画面を離れずに答える。
// 依頼の出来事で一覧を読み直すので、開いているあいだに来た依頼も出る。
export function WorkspaceAsks({ workspace, onClose }: WorkspaceOverlayProps) {
  const narrow = useMediaQuery('(max-width: 47.99em)') ?? true;
  const asks = useQuery(asksQuery());
  const mine = asks.data?.filter((ask) => ask.workspace === workspace);
  return (
    <Drawer
      opened
      onClose={onClose}
      position={narrow ? 'bottom' : 'right'}
      size={narrow ? '85%' : 'md'}
      title={
        <Text fw={600} truncate>
          Asks from{' '}
          <Text span ff="monospace" inherit>
            {workspace}
          </Text>
        </Text>
      }
    >
      <Stack>
        <QueryState
          pending={asks.isPending}
          error={asks.error}
          empty={mine?.length === 0}
          emptyText="No asks are waiting for an answer."
        />
        {mine?.map((ask) => (
          <AskCard key={ask.id} ask={ask} />
        ))}
        <Anchor component={Link} to="/inbox" size="sm">
          Open the inbox
        </Anchor>
      </Stack>
    </Drawer>
  );
}

// 一つのワークスペースの答えを待つ依頼の数 (なければ 0)。
export function useWorkspacePendingAsks(workspace: string): number {
  return useQuery(asksQuery()).data?.filter((ask) => ask.workspace === workspace).length ?? 0;
}
