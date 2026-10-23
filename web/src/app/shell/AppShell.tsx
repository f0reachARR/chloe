import {
  Badge,
  Box,
  Group,
  Menu,
  AppShell as MantineAppShell,
  Text,
  Tooltip,
  UnstyledButton,
} from '@mantine/core';
import { useMediaQuery } from '@mantine/hooks';
import { IconChevronDown, IconLogout } from '@tabler/icons-react';
import { useQuery } from '@tanstack/react-query';
import { Outlet, useMatches } from '@tanstack/react-router';
import { useEffect } from 'react';

import { meQuery, signOut } from '@/features/auth';
import { asksQuery } from '@/features/inbox';
import type { LiveState } from '@/shared/events';

import { useFeatureEvents } from '../events';
import { navItems } from '../features';
import { BottomNavigation, SideNavigation } from './Navigation';

const liveLabel: Record<LiveState, { color: string; text: string; hint: string }> = {
  live: { color: 'teal', text: 'Live', hint: 'New asks appear as they come in' },
  connecting: { color: 'yellow', text: 'Connecting', hint: 'Connecting for live updates' },
  offline: { color: 'gray', text: 'Offline', hint: 'Live updates stopped; retrying' },
};

// ログインしたあとの枠。広い画面では左に、スマートフォンの幅では下にナビゲーションを置く。
export function AppShell() {
  const me = useQuery(meQuery());
  const live = useFeatureEvents();
  const pending = useQuery(asksQuery()).data?.length ?? 0;
  const wide = useMatches({ select: (matches) => matches.some((m) => m.staticData.wide) });

  useEffect(() => {
    document.title = pending > 0 ? `(${pending}) chloe` : 'chloe';
  }, [pending]);

  // スマートフォンの幅 (Mantine の sm より狭い) では下に、広い画面では左にナビゲーションを置く。
  // 見えない側は描かない (画面の外のリンクにフォーカスが移らないように)。
  const narrow = useMediaQuery('(max-width: 47.99em)') ?? true;
  const state = liveLabel[live];
  return (
    <MantineAppShell
      header={{ height: 56 }}
      navbar={narrow ? undefined : { width: 220, breakpoint: 'sm' }}
      footer={narrow ? { height: 60 } : undefined}
      padding="md"
    >
      <MantineAppShell.Header>
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
                    {me.data?.login}
                  </Text>
                  <IconChevronDown size={14} />
                </Group>
              </UnstyledButton>
            </Menu.Target>
            <Menu.Dropdown>
              <Menu.Item leftSection={<IconLogout size={16} />} onClick={() => void signOut()}>
                Sign out
              </Menu.Item>
            </Menu.Dropdown>
          </Menu>
        </Group>
      </MantineAppShell.Header>

      {!narrow && (
        <MantineAppShell.Navbar p="xs">
          <SideNavigation items={navItems} />
        </MantineAppShell.Navbar>
      )}

      <MantineAppShell.Main>
        <Box maw={wide ? undefined : 760} mx="auto">
          <Outlet />
        </Box>
      </MantineAppShell.Main>

      {narrow && (
        <MantineAppShell.Footer>
          <BottomNavigation items={navItems} />
        </MantineAppShell.Footer>
      )}
    </MantineAppShell>
  );
}
