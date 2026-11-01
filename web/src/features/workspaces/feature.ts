import { IconCpu, IconPlugConnected, IconServer } from '@tabler/icons-react';

import type { Feature } from '@/shared/feature';

import { reloadWorkspaces } from './api';
import { MachineSection } from './MachineSection';
import { PortsSection } from './PortsSection';

export const workspacesFeature: Feature = {
  name: 'workspaces',
  nav: [{ to: '/workspaces', label: 'Workspaces', icon: IconServer }],
  workspaceSettings: [
    { id: 'machine', label: 'Machine', icon: IconCpu, order: 10, component: MachineSection },
    { id: 'ports', label: 'Ports', icon: IconPlugConnected, order: 20, component: PortsSection },
  ],
  events: {
    // 答えを待つ依頼の数が変わる。
    ask: (_, { queryClient }) => void reloadWorkspaces(queryClient),
    resync: (_, { queryClient }) => void reloadWorkspaces(queryClient),
  },
};
