import { Stack, Switch } from '@mantine/core';
import { useQuery } from '@tanstack/react-query';

import { QueryState } from '@/shared/ui/QueryState';
import { Toolbar } from '@/shared/ui/Toolbar';

import { asksQuery } from './api';
import { AskCard } from './AskCard';

// 依頼の一覧。答えたものも出すか (`all`) は URL に置く。
export function InboxPage({
  all,
  onAllChange,
}: {
  all: boolean;
  onAllChange: (all: boolean) => void;
}) {
  const asks = useQuery(asksQuery(all));
  return (
    <Stack>
      <Toolbar reloading={asks.isFetching} onReload={() => asks.refetch()}>
        <Switch
          label="Show answered"
          checked={all}
          onChange={(event) => onAllChange(event.currentTarget.checked)}
        />
      </Toolbar>
      <QueryState
        pending={asks.isPending}
        error={asks.error}
        empty={asks.data?.length === 0}
        emptyText={all ? 'No asks yet.' : 'No asks are waiting for an answer.'}
      />
      {asks.data?.map((ask) => (
        <AskCard key={ask.id} ask={ask} />
      ))}
    </Stack>
  );
}
