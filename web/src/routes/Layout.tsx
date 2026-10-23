import {
  AppShell,
  Badge,
  Box,
  Center,
  Group,
  Loader,
  Menu,
  Tabs,
  Text,
  Tooltip,
  UnstyledButton,
} from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconChevronDown, IconInbox, IconLogout, IconServer } from '@tabler/icons-react';
import { useCallback, useEffect } from 'react';
import { Outlet, useLocation, useNavigate } from 'react-router';

import { type AskEventData, api, fetchClient } from '../api/client';
import { type LiveState, useAskEvents } from '../api/events';

const liveLabel: Record<LiveState, { color: string; text: string; hint: string }> = {
  live: { color: 'teal', text: 'Live', hint: 'New asks appear as they come in' },
  connecting: { color: 'yellow', text: 'Connecting', hint: 'Connecting for live updates' },
  offline: { color: 'gray', text: 'Offline', hint: 'Live updates stopped; retrying' },
};

export function Layout() {
  const navigate = useNavigate();
  const location = useLocation();
  const me = api.useQuery('get', '/api/v1/me');
  const pending = api.useQuery('get', '/api/v1/asks', {}, { enabled: me.isSuccess });
  const count = pending.data?.length ?? 0;

  const onCreated = useCallback(
    (event: AskEventData) => {
      notifications.show({
        title: 'New ask',
        message: `From ${event.workspace}`,
        onClick: () => navigate('/inbox'),
      });
    },
    [navigate],
  );
  const live = useAskEvents(onCreated);

  useEffect(() => {
    document.title = count > 0 ? `(${count}) chloe` : 'chloe';
  }, [count]);

  if (!me.data) {
    return (
      <Center mih="100dvh">
        <Loader />
      </Center>
    );
  }

  const signOut = async () => {
    await fetchClient.POST('/api/auth/logout');
    window.location.assign('/login');
  };
  const tab = location.pathname.startsWith('/workspaces') ? 'workspaces' : 'inbox';
  const state = liveLabel[live];

  return (
    <AppShell header={{ height: 56 }} padding="md">
      <AppShell.Header>
        <Group h="100%" px="md" justify="space-between" wrap="nowrap">
          <Group gap="sm" wrap="nowrap">
            <img src="/icons/icon.svg" alt="" width={28} height={28} />
            <Text fw={700} size="lg">
              chloe
            </Text>
            <Tooltip label={state.hint}>
              <Badge variant="dot" color={state.color} size="sm">
                {state.text}
              </Badge>
            </Tooltip>
          </Group>
          <Menu position="bottom-end" withinPortal>
            <Menu.Target>
              <UnstyledButton>
                <Group gap={4} wrap="nowrap">
                  <Text size="sm" fw={500} truncate maw={140}>
                    {me.data.login}
                  </Text>
                  <IconChevronDown size={14} />
                </Group>
              </UnstyledButton>
            </Menu.Target>
            <Menu.Dropdown>
              <Menu.Item leftSection={<IconLogout size={16} />} onClick={signOut}>
                Sign out
              </Menu.Item>
            </Menu.Dropdown>
          </Menu>
        </Group>
      </AppShell.Header>

      <AppShell.Main>
        <Box maw={760} mx="auto">
          <Tabs value={tab} onChange={(value) => value && navigate(`/${value}`)} mb="md">
            <Tabs.List>
              <Tabs.Tab
                value="inbox"
                leftSection={<IconInbox size={16} />}
                rightSection={
                  count > 0 ? (
                    <Badge size="sm" circle>
                      {count}
                    </Badge>
                  ) : undefined
                }
              >
                Inbox
              </Tabs.Tab>
              <Tabs.Tab value="workspaces" leftSection={<IconServer size={16} />}>
                Workspaces
              </Tabs.Tab>
            </Tabs.List>
          </Tabs>
          <Outlet />
        </Box>
      </AppShell.Main>
    </AppShell>
  );
}
