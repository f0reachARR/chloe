import { screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { workspace } from '@/test/fixtures';
import { http, server } from '@/test/msw';
import { renderWithProviders } from '@/test/render';

import { WorkspacesPage } from './WorkspacesPage';

describe('WorkspacesPage', () => {
  it('shows what each workspace is doing', async () => {
    server.use(
      http.get('/api/v1/workspaces', ({ query, response }) => {
        expect(query.get('status')).toBe('true');
        return response(200).json([
          workspace({ name: 'default', busy: ['claude-1', 'claude-2'], pending_asks: 1 }),
          workspace({ name: 'acme/proj', busy: [] }),
          workspace({ name: 'api', busy: null }),
          workspace({ name: 'old', running: false }),
        ]);
      }),
    );
    renderWithProviders(<WorkspacesPage />);
    expect(await screen.findByText('Working: claude ×2')).toBeInTheDocument();
    expect(screen.getByText('1 waiting')).toBeInTheDocument();
    expect(screen.getByText('Running · idle')).toBeInTheDocument();
    expect(screen.getByText('Running · status unknown')).toBeInTheDocument();
    expect(screen.getByText('Stopped')).toBeInTheDocument();
  });
});
