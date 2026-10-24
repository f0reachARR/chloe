import { notifications } from '@mantine/notifications';
import { IconInbox } from '@tabler/icons-react';

import type { Schemas } from '@/shared/api/client';
import type { Feature } from '@/shared/feature';
import { openOverlay } from '@/shared/overlays';

import { reloadAsks } from './api';
import { PendingBadge } from './PendingBadge';
import { WorkspaceAsks } from './WorkspaceAsks';

export const inboxFeature: Feature = {
  name: 'inbox',
  nav: [{ to: '/inbox', label: 'Inbox', icon: IconInbox, badge: PendingBadge }],
  overlays: { asks: WorkspaceAsks },
  events: {
    ask: (data, { queryClient, navigate }) => {
      void reloadAsks(queryClient);
      const event = data as Schemas['AskEventData'];
      if (event.type === 'created') {
        notifications.show({
          title: 'New ask',
          message: `From ${event.workspace}`,
          // 今の画面 (端末など) を離れずに答えられるよう、その上に開く。
          onClick: () => navigate({ to: '.', search: openOverlay('asks', event.workspace) }),
        });
      }
    },
    resync: (_, { queryClient }) => void reloadAsks(queryClient),
  },
};
