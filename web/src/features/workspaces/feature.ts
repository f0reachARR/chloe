import { IconServer } from '@tabler/icons-react';

import type { Feature } from '@/shared/feature';

import { reloadWorkspaces } from './api';

export const workspacesFeature: Feature = {
  name: 'workspaces',
  nav: [{ to: '/workspaces', label: 'Workspaces', icon: IconServer }],
  events: {
    // 答えを待つ依頼の数が変わる。
    ask: (_, { queryClient }) => void reloadWorkspaces(queryClient),
    resync: (_, { queryClient }) => void reloadWorkspaces(queryClient),
  },
};
