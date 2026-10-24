import { screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { networkAsk, questionAsk } from '@/test/fixtures';
import { http, server } from '@/test/msw';
import { renderWithProviders } from '@/test/render';

import { WorkspaceAsks } from './WorkspaceAsks';

describe('WorkspaceAsks', () => {
  it('shows only the asks from one workspace', async () => {
    server.use(
      http.get('/api/v1/asks', ({ response }) =>
        response(200).json([
          networkAsk({ id: 1, workspace: 'acme/proj' }),
          questionAsk({ id: 2, workspace: 'api' }),
        ]),
      ),
    );
    renderWithProviders(<WorkspaceAsks workspace="acme/proj" onClose={() => {}} />);
    expect(await screen.findByText('docs.example.com')).toBeInTheDocument();
    expect(screen.queryByText('Which branch?')).not.toBeInTheDocument();
  });

  it('says when nothing is waiting', async () => {
    server.use(
      http.get('/api/v1/asks', ({ response }) =>
        response(200).json([questionAsk({ workspace: 'api' })]),
      ),
    );
    renderWithProviders(<WorkspaceAsks workspace="acme/proj" onClose={() => {}} />);
    expect(await screen.findByText('No asks are waiting for an answer.')).toBeInTheDocument();
  });
});
