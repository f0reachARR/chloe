import { IconShieldLock } from '@tabler/icons-react';

import type { Feature } from '@/shared/feature';

import { reloadPolicies, reloadRules } from './api';
import { PolicySection } from './PolicySection';

export const policiesFeature: Feature = {
  name: 'policies',
  nav: [{ to: '/policies', label: 'Policies', icon: IconShieldLock }],
  workspaceSettings: [
    { id: 'policy', label: 'Policy', icon: IconShieldLock, order: 30, component: PolicySection },
  ],
  events: {
    // 通信の許可を承認すると、ワークスペースの規則に足される。
    ask: (_, { queryClient }) => void reloadRules(queryClient),
    resync: (_, { queryClient }) => {
      void reloadRules(queryClient);
      void reloadPolicies(queryClient);
    },
  },
};
