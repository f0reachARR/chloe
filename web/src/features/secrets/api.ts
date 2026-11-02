import type { QueryClient } from '@tanstack/react-query';

import { api, invalidatePath } from '@/shared/api/client';
import type { OwnerEntry } from '@/shared/owners';

// シークレットの `owner=`。個人のものは省く (`alice` は Organization の名前と区別できない)。
export const ownerQuery = (owner: Pick<OwnerEntry, 'kind' | 'name'>) =>
  owner.kind === 'user' ? {} : { owner: owner.name };

export const secretsQuery = (owner: Pick<OwnerEntry, 'kind' | 'name'>) =>
  api.queryOptions('get', '/api/v1/secrets', { params: { query: ownerQuery(owner) } });

export const templatesQuery = () =>
  api.queryOptions('get', '/api/v1/secret-templates', {}, { staleTime: Infinity });

export const reloadSecrets = (queryClient: QueryClient) =>
  invalidatePath(queryClient, '/api/v1/secrets');
