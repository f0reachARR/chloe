import type { QueryClient } from '@tanstack/react-query';

import { api, invalidatePath } from '@/shared/api/client';

// ワークスペースで効く規則、ワークスペース自身の規則とその版、変えられるか。
export const rulesQuery = (workspace: string) =>
  api.queryOptions('get', '/api/v1/workspace-rules/{name}', {
    params: { path: { name: workspace } },
  });

export const reloadRules = (queryClient: QueryClient) =>
  invalidatePath(queryClient, '/api/v1/workspace-rules/{name}');

// 利用者が見られる設定の層 (ゲートウェイ、個人、Organization、team) と、その持ち主のワークスペース。
export const scopesQuery = () => api.queryOptions('get', '/api/v1/policy-scopes');

// 層で効く規則、層の設定とその版、変えられるか。
export const policyRulesQuery = (scope: string) =>
  api.queryOptions('get', '/api/v1/policy-rules', { params: { query: { scope } } });

export const reloadPolicies = (queryClient: QueryClient) =>
  Promise.all([
    invalidatePath(queryClient, '/api/v1/policy-scopes'),
    invalidatePath(queryClient, '/api/v1/policy-rules'),
  ]);
