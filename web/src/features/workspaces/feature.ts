import { IconCpu, IconPlugConnected, IconServer, IconTrash } from '@tabler/icons-react';

import type { Feature } from '@/shared/feature';

import { reloadWorkspaces } from './api';
import { DeleteSection } from './DeleteSection';
import { MachineSection } from './MachineSection';
import { PortsSection } from './PortsSection';

export const workspacesFeature: Feature = {
  name: 'workspaces',
  nav: [{ to: '/workspaces', label: 'Workspaces', icon: IconServer }],
  workspaceSettings: [
    { id: 'machine', label: 'Machine', icon: IconCpu, order: 10, component: MachineSection },
    { id: 'ports', label: 'Ports', icon: IconPlugConnected, order: 20, component: PortsSection },
    // ほかの機能のセクションより後ろに置く。
    { id: 'delete', label: 'Delete', icon: IconTrash, order: 1000, component: DeleteSection },
  ],
  events: {
    // 答えを待つ依頼の数が変わる。
    ask: (_, { queryClient }) => void reloadWorkspaces(queryClient),
    resync: (_, { queryClient }) => void reloadWorkspaces(queryClient),
  },
};
