import { screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import { workspace } from '@/test/fixtures';
import { http, server } from '@/test/msw';
import { renderWithProviders } from '@/test/render';

import { WorkspacesPage } from './WorkspacesPage';

describe('creating a workspace', () => {
  it('creates it under the chosen team and shows the new card', async () => {
    let list = [workspace({ name: 'default' })];
    const bodies: unknown[] = [];
    server.use(
      http.get('/api/v1/workspaces', ({ response }) => response(200).json(list)),
      http.get('/api/v1/workspace-ports/{name}', ({ response }) => response(200).json([])),
      http.get('/api/v1/owners', ({ response }) =>
        response(200).json([
          {
            owner: 'user:alice',
            kind: 'user',
            name: 'alice',
            prefix: '',
            can_manage_secrets: true,
          },
          {
            owner: 'org:acme',
            kind: 'org',
            name: 'acme',
            prefix: 'acme/',
            can_manage_secrets: true,
          },
          {
            owner: 'team:acme/dev',
            kind: 'team',
            name: 'acme/dev',
            prefix: 'acme/dev/',
            can_manage_secrets: true,
          },
        ]),
      ),
      http.post('/api/v1/workspaces', async ({ request, response }) => {
        const body = await request.json();
        bodies.push(body);
        const created = workspace({ name: body.name, running: false });
        list = [...list, created];
        return response(201).json(created);
      }),
    );
    renderWithProviders(<WorkspacesPage />);
    await userEvent.click(await screen.findByRole('button', { name: 'New workspace' }));
    const dialog = await screen.findByRole('dialog');

    await userEvent.click(within(dialog).getAllByLabelText('Owner')[0]);
    await userEvent.click(await screen.findByRole('option', { name: 'acme/dev (team)' }));
    await userEvent.type(within(dialog).getByRole('textbox', { name: 'Name' }), 'Web');
    expect(within(dialog).getByText('acme/dev/web')).toBeInTheDocument();

    await userEvent.click(within(dialog).getByRole('button', { name: 'Create' }));
    await expect.poll(() => bodies).toEqual([{ name: 'acme/dev/web' }]);
    expect(await screen.findByText('acme/dev/web', { selector: 'p' })).toBeInTheDocument();
  });

  it('checks the name before sending', async () => {
    server.use(
      http.get('/api/v1/workspaces', ({ response }) => response(200).json([])),
      http.get('/api/v1/owners', ({ response }) =>
        response(200).json([
          {
            owner: 'user:alice',
            kind: 'user',
            name: 'alice',
            prefix: '',
            can_manage_secrets: true,
          },
        ]),
      ),
    );
    renderWithProviders(<WorkspacesPage />);
    await userEvent.click(await screen.findByRole('button', { name: 'New workspace' }));
    const dialog = await screen.findByRole('dialog');
    await userEvent.type(within(dialog).getByRole('textbox', { name: 'Name' }), '-bad');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Create' }));
    expect(await within(dialog).findByText(/not starting with -/)).toBeInTheDocument();
  });
});
