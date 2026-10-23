import { notifications } from '@mantine/notifications';
import { IconInbox } from '@tabler/icons-react';

import type { Schemas } from '@/shared/api/client';
import type { Feature } from '@/shared/feature';

import { reloadAsks } from './api';
import { PendingBadge } from './PendingBadge';

export const inboxFeature: Feature = {
  name: 'inbox',
  nav: [{ to: '/inbox', label: 'Inbox', icon: IconInbox, badge: PendingBadge }],
  events: {
    ask: (data, { queryClient, navigate }) => {
      void reloadAsks(queryClient);
      const event = data as Schemas['AskEventData'];
      if (event.type === 'created') {
        notifications.show({
          title: 'New ask',
          message: `From ${event.workspace}`,
          onClick: () => navigate('/inbox'),
        });
      }
    },
    resync: (_, { queryClient }) => void reloadAsks(queryClient),
  },
};
