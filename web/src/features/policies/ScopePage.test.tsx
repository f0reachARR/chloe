import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import type { Schemas } from '@/shared/api/client';
import { http, server } from '@/test/msw';
import { renderWithProviders } from '@/test/render';

import { PoliciesPage } from './PoliciesPage';
import { ScopePage } from './ScopePage';

type PolicyRules = Schemas['PolicyRules'];
type PolicyLayer = Schemas['PolicyLayer'];

const gatewayRule = { hosts: ['github.com'], action: 'allow' as const };

// 層の設定を置き換えるたびに版が変わる偽の API。
function fakeLayer({ own = {} as PolicyLayer, editable = true } = {}) {
  let current = own;
  let version = 'v1';
  const puts: { path: string; ifMatch: string | null; body: unknown }[] = [];
  const rules = (owner: string): PolicyRules => ({
    rules: [
      { source: { kind: 'gateway' }, rule: gatewayRule },
      ...(current.rules ?? []).map((rule) => ({ source: { kind: 'owner' as const, owner }, rule })),
    ],
    own: current,
    editable,
    version,
  });
  const put = async (path: string, request: Request) => {
    const body = (await request.json()) as PolicyLayer;
    const ifMatch = request.headers.get('If-Match');
    puts.push({ path, ifMatch, body });
    current = body;
    version = `v${Number(version.slice(1)) + 1}`;
  };
  server.use(
    http.get('/api/v1/policy-rules', ({ query, response }) => {
      const scope = query.get('scope') ?? '';
      return response(200).json(rules(scope.slice(scope.indexOf(':') + 1)));
    }),
    http.get('/api/v1/policy-scopes', ({ response }) => response(200).json([])),
    http.put('/api/v1/policies/{owner}', async ({ params, request, response }) => {
      await put(`policies/${params.owner}`, request);
      return response(200).json({ updated_runners: 2 });
    }),
    http.put('/api/v1/policy', async ({ request, response }) => {
      await put('policy', request);
      return response(200).json({ updated_runners: 0 });
    }),
  );
  return puts;
}

describe('ScopePage', () => {
  it('saves a team default root and token scope with the version it read', async () => {
    const puts = fakeLayer();
    renderWithProviders(<ScopePage scope="team:acme/dev" />);
    await userEvent.click(await screen.findByRole('radio', { name: 'Persistent' }));
    await userEvent.click(screen.getByRole('radio', { name: 'Limit' }));
    await userEvent.click(screen.getAllByRole('radio', { name: 'Only these' })[1]);
    await userEvent.click(screen.getByRole('button', { name: 'Add permission' }));
    await userEvent.type(screen.getByRole('combobox', { name: 'Permission 1' }), 'contents');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    await expect
      .poll(() => puts)
      .toEqual([
        {
          path: 'policies/acme/dev',
          ifMatch: 'v1',
          body: { root: 'persistent', github: { permissions: { contents: 'read' } } },
        },
      ]);
  });

  it('saves personal settings without a token scope', async () => {
    const puts = fakeLayer();
    renderWithProviders(<ScopePage scope="user:alice" />);
    await screen.findByText('Default root for new workspaces');
    expect(screen.queryByText('GitHub token scope')).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Add rule' }));
    await userEvent.type(
      screen.getByRole('combobox', { name: 'Hosts of rule 1' }),
      'example.org{Enter}',
    );
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    await expect
      .poll(() => puts)
      .toEqual([
        {
          path: 'policy',
          ifMatch: 'v1',
          body: { rules: [{ hosts: ['example.org'], action: 'allow' }] },
        },
      ]);
  });

  it('shows read-only settings to people who cannot change them', async () => {
    fakeLayer({
      own: { root: 'ephemeral', github: { repositories: ['web'] } },
      editable: false,
    });
    renderWithProviders(<ScopePage scope="org:acme" />);
    expect(await screen.findByText(/You can view but not change/)).toBeInTheDocument();
    expect(screen.getByText('ephemeral')).toBeInTheDocument();
    expect(screen.getByText('web; all permissions')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Save' })).not.toBeInTheDocument();
  });
});

describe('PoliciesPage', () => {
  it('lists the layers and links workspaces to their settings', async () => {
    server.use(
      http.get('/api/v1/policy-scopes', ({ response }) =>
        response(200).json([
          {
            scope: 'gateway',
            kind: 'gateway',
            name: 'gateway',
            editable: false,
            rules: 3,
            workspaces: [],
          },
          {
            scope: 'user:alice',
            kind: 'user',
            name: 'alice',
            editable: false,
            rules: 0,
            workspaces: ['default'],
          },
          {
            scope: 'org:acme',
            kind: 'org',
            name: 'acme',
            editable: true,
            rules: 1,
            workspaces: ['acme/proj'],
          },
        ]),
      ),
    );
    renderWithProviders(<PoliciesPage />);
    expect(await screen.findByRole('link', { name: 'acme' })).toHaveAttribute(
      'href',
      '/policies?scope=org%3Aacme',
    );
    expect(screen.getByText('Organization')).toBeInTheDocument();
    expect(screen.getByText('1 rule')).toBeInTheDocument();
    expect(screen.getByText('3 rules')).toBeInTheDocument();
    expect(screen.getByLabelText('You can change these settings')).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'acme/proj' })).toHaveAttribute(
      'href',
      expect.stringContaining('settings=acme%2Fproj'),
    );
  });
});
