import { NavLink, Stack, Text, UnstyledButton } from '@mantine/core';
import { Link, useRouterState } from '@tanstack/react-router';

import type { NavItem } from '@/shared/feature';

import classes from './Navigation.module.css';

function useActive(item: NavItem): boolean {
  const pathname = useRouterState({ select: (state) => state.location.pathname });
  return typeof item.to === 'string' && pathname.startsWith(item.to);
}

// 広い画面の左のナビゲーション。
function SideItem({ item }: { item: NavItem }) {
  const { icon: Icon, badge: ItemBadge } = item;
  return (
    <NavLink
      component={Link}
      to={item.to}
      label={item.label}
      leftSection={<Icon size={18} />}
      rightSection={ItemBadge ? <ItemBadge /> : undefined}
      active={useActive(item)}
    />
  );
}

export function SideNavigation({ items }: { items: NavItem[] }) {
  return (
    <Stack gap={2}>
      {items.map((item) => (
        <SideItem key={item.label} item={item} />
      ))}
    </Stack>
  );
}

// スマートフォンの幅の下のナビゲーション。
function BottomItem({ item }: { item: NavItem }) {
  const { icon: Icon, badge: ItemBadge } = item;
  const active = useActive(item);
  return (
    <UnstyledButton
      component={Link}
      to={item.to}
      className={classes.bottomItem}
      data-active={active || undefined}
    >
      <span className={classes.iconWrap}>
        <Icon size={22} />
        {ItemBadge && (
          <span className={classes.badge}>
            <ItemBadge />
          </span>
        )}
      </span>
      <Text size="xs" fw={active ? 600 : 400}>
        {item.label}
      </Text>
    </UnstyledButton>
  );
}

export function BottomNavigation({ items }: { items: NavItem[] }) {
  return (
    <nav className={classes.bottom}>
      {items.map((item) => (
        <BottomItem key={item.label} item={item} />
      ))}
    </nav>
  );
}
