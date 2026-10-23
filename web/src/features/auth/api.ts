import { api, fetchClient } from '@/shared/api/client';

export const meQuery = () => api.queryOptions('get', '/api/v1/me');
export const methodsQuery = () => api.queryOptions('get', '/api/auth/methods');

export async function signOut(): Promise<void> {
  await fetchClient.POST('/api/auth/logout');
  window.location.assign('/login');
}
