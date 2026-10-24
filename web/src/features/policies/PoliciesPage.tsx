import { Anchor, Badge, Card, Group, Stack, Text } from '@mantine/core';
import { IconPencil } from '@tabler/icons-react';
import { useQuery } from '@tanstack/react-query';
import { Link } from '@tanstack/react-router';

import type { Schemas } from '@/shared/api/client';
import { openOverlay } from '@/shared/overlays';
import { QueryState } from '@/shared/ui/QueryState';
import { Toolbar } from '@/shared/ui/Toolbar';

import { scopesQuery } from './api';
import { kindLabel } from './layer';

function ScopeCard({ scope }: { scope: Schemas['PolicyScope'] }) {
  return (
    <Card withBorder padding="md">
      <Stack gap="xs">
        <Group justify="space-between" wrap="nowrap">
          <Group gap="xs" wrap="nowrap" miw={0}>
            <Anchor
              // component={Link} では Link の検索パラメーターの型が失われるので、Link を直に描く。
              renderRoot={(props) => (
                <Link to="/policies" search={{ scope: scope.scope }} {...props} />
              )}
              fw={600}
              ff="monospace"
              truncate
            >
              {scope.name}
            </Anchor>
            <Badge size="sm" variant="light" color={scope.kind === 'gateway' ? 'gray' : 'blue'}>
              {kindLabel[scope.kind]}
            </Badge>
          </Group>
          <Group gap={6} wrap="nowrap">
            {scope.editable && <IconPencil size={14} aria-label="You can change these settings" />}
            <Text size="sm" c="dimmed" style={{ whiteSpace: 'nowrap' }}>
              {scope.rules} {scope.rules === 1 ? 'rule' : 'rules'}
            </Text>
          </Group>
        </Group>
        {scope.workspaces.length > 0 && (
          <Group gap={6}>
            {scope.workspaces.map((ws) => (
              <Badge
                key={ws}
                variant="outline"
                color="gray"
                tt="none"
                style={{ cursor: 'pointer' }}
                // そのワークスペースの規則は、ワークスペースの設定の Policy のタブで変える。
                renderRoot={(props) => (
                  <Link to="." search={openOverlay('settings', ws, 'policy')} {...props} />
                )}
              >
                {ws}
              </Badge>
            ))}
          </Group>
        )}
      </Stack>
    </Card>
  );
}

// 利用者が見られる設定の層。上の層から順に並ぶ (上の層の規則を、下の層は覆せない)。
export function PoliciesPage() {
  const scopes = useQuery(scopesQuery());
  return (
    <Stack>
      <Toolbar reloading={scopes.isFetching} onReload={() => scopes.refetch()}>
        <Text size="xs" c="dimmed">
          Layers are checked from the top. Tap a workspace to change its own rules.
        </Text>
      </Toolbar>
      <QueryState
        pending={scopes.isPending}
        error={scopes.error}
        empty={scopes.data?.length === 0}
        emptyText="No settings to show."
      />
      {scopes.data?.map((scope) => (
        <ScopeCard key={scope.scope} scope={scope} />
      ))}
    </Stack>
  );
}
