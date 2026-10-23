import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
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
      http.get('/api/v1/workspace-ports/{name}', ({ response }) => response(200).json([])),
    );
    renderWithProviders(<WorkspacesPage />);
    expect(await screen.findByText('Working: claude ×2')).toBeInTheDocument();
    expect(screen.getByText('1 waiting')).toBeInTheDocument();
    expect(screen.getByText('Running · idle')).toBeInTheDocument();
    expect(screen.getByText('Running · status unknown')).toBeInTheDocument();
    expect(screen.getByText('Stopped')).toBeInTheDocument();
  });

  it('links to the published ports and unpublishes them', async () => {
    let published = [
      {
        port: 5173,
        scheme: 'http' as const,
        url: 'https://5173-ws-1.preview.example.com/',
        created_at: 0,
      },
      {
        port: 8443,
        scheme: 'https' as const,
        url: 'https://8443-ws-1.preview.example.com/',
        created_at: 0,
      },
    ];
    server.use(
      http.get('/api/v1/workspaces', ({ response }) =>
        response(200).json([workspace({ name: 'acme/proj' })]),
      ),
      http.get('/api/v1/workspace-ports/{name}', ({ params, response }) => {
        expect(params.name).toBe('acme/proj');
        return response(200).json(published);
      }),
      http.delete('/api/v1/workspace-ports/{name}', ({ query, response }) => {
        published = published.filter((p) => String(p.port) !== query.get('port'));
        return response(204).empty();
      }),
    );
    renderWithProviders(<WorkspacesPage />);
    const link = await screen.findByRole('link', { name: /5173/ });
    expect(link).toHaveAttribute('href', 'https://5173-ws-1.preview.example.com/');
    expect(link).toHaveAttribute('target', '_blank');
    expect(screen.getByRole('link', { name: /8443 \(https\)/ })).toBeInTheDocument();

    await userEvent.click(screen.getByRole('button', { name: 'Unpublish port 5173' }));
    await waitFor(() =>
      expect(screen.queryByRole('link', { name: /5173/ })).not.toBeInTheDocument(),
    );
    expect(screen.getByRole('link', { name: /8443/ })).toBeInTheDocument();
  });
});
