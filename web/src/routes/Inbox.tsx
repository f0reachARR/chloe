import { Alert, Button, Group, Loader, Stack, Switch, Text } from '@mantine/core';
import { IconRefresh } from '@tabler/icons-react';
import { useState } from 'react';

import { api, errorMessage } from '../api/client';
import { AskCard } from '../components/AskCard';

export function Inbox() {
  const [all, setAll] = useState(false);
  const asks = api.useQuery('get', '/api/v1/asks', { params: { query: all ? { all } : {} } });

  return (
    <Stack>
      <Group justify="space-between">
        <Switch
          label="Show answered"
          checked={all}
          onChange={(event) => setAll(event.currentTarget.checked)}
        />
        <Button
          variant="subtle"
          size="compact-sm"
          leftSection={<IconRefresh size={14} />}
          loading={asks.isFetching}
          onClick={() => asks.refetch()}
        >
          Reload
        </Button>
      </Group>
      {asks.isPending && (
        <Group justify="center" py="xl">
          <Loader />
        </Group>
      )}
      {asks.error && <Alert color="red">{errorMessage(asks.error)}</Alert>}
      {asks.data?.length === 0 && (
        <Text c="dimmed" ta="center" py="xl">
          {all ? 'No asks yet.' : 'No asks are waiting for an answer.'}
        </Text>
      )}
      {asks.data?.map((ask) => (
        <AskCard key={ask.id} ask={ask} />
      ))}
    </Stack>
  );
}
