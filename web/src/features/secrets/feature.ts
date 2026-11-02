import { IconKey } from '@tabler/icons-react';

import type { Feature } from '@/shared/feature';

import { reloadSecrets } from './api';

export const secretsFeature: Feature = {
  name: 'secrets',
  nav: [{ to: '/secrets', label: 'Secrets', icon: IconKey }],
  events: {
    resync: (_, { queryClient }) => void reloadSecrets(queryClient),
  },
};
