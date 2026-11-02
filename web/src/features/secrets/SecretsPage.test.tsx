import { screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import type { Schemas } from '@/shared/api/client';
import { http, server } from '@/test/msw';
import { renderWithProviders } from '@/test/render';

import { SecretsPage } from './SecretsPage';

const owners: Schemas['OwnerEntry'][] = [
  { owner: 'user:alice', kind: 'user', name: 'alice', prefix: '', can_manage_secrets: true },
  { owner: 'org:acme', kind: 'org', name: 'acme', prefix: 'acme/', can_manage_secrets: false },
  {
    owner: 'team:acme/dev',
    kind: 'team',
    name: 'acme/dev',
    prefix: 'acme/dev/',
    can_manage_secrets: true,
  },
];

const entry = (name: string, owner: string, hosts: string[]): Schemas['SecretView'] => ({
  name,
  owner,
  builtin: false,
  spec: {
    fields: { value: { env: [name.toUpperCase()], placeholder: '{id}' } },
    inject: [{ hosts, header: 'authorization' }],
  },
});

const templates: Schemas['SecretTemplate'][] = [
  {
    name: 'anthropic_api_key',
    spec: {
      fields: { value: { env: ['ANTHROPIC_API_KEY'], placeholder: 'sk-ant-api03-{id}' } },
      inject: [{ hosts: ['api.anthropic.com'], header: 'x-api-key' }],
    },
  },
];

// 送った本文と `owner=` を残す偽の API。
function fakeApi(status = 204) {
  const puts: { name: string; owner: string | null; body: unknown }[] = [];
  server.use(
    http.get('/api/v1/owners', ({ response }) => response(200).json(owners)),
    http.get('/api/v1/secret-templates', ({ response }) => response(200).json(templates)),
    http.get('/api/v1/secrets', ({ query, response }) => {
      const owner = query.get('owner');
      const entries =
        owner === 'acme/dev'
          ? [entry('npm_token', 'acme/dev', ['registry.npmjs.org'])]
          : owner === 'acme'
            ? [
                entry('npm_token', 'acme', ['registry.npmjs.org']),
                entry('hf_token', 'acme', ['huggingface.co']),
              ]
            : [];
      return response(200).json({
        owner: owner ?? 'alice',
        names: entries.map((e) => e.name),
        entries,
      });
    }),
    http.put('/api/v1/secrets/{name}', async ({ params, query, request, response }) => {
      puts.push({ name: params.name, owner: query.get('owner'), body: await request.json() });
      return status === 204
        ? response(204).empty()
        : response('4XX').json({ error: 'the value is wrong' }, { status: 400 });
    }),
  );
  return puts;
}

describe('SecretsPage', () => {
  it("lists a team's secrets with the organization's ones it falls back to", async () => {
    fakeApi();
    renderWithProviders(<SecretsPage owner="team:acme/dev" onOwnerChange={() => {}} />);
    expect(await screen.findByText('hf_token')).toBeInTheDocument();
    expect(screen.getByText('from acme')).toBeInTheDocument();
    // team にある名前は、Organization のものを並べない。
    expect(screen.getAllByText('npm_token')).toHaveLength(1);
    expect(screen.getByRole('button', { name: 'Change npm_token' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Change hf_token' })).not.toBeInTheDocument();
  });

  it('is read-only for people who cannot change the secrets', async () => {
    fakeApi();
    renderWithProviders(<SecretsPage owner="org:acme" onOwnerChange={() => {}} />);
    expect(await screen.findByText('hf_token')).toBeInTheDocument();
    expect(screen.getByText(/Only organization admins can change these/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /New secret/ })).not.toBeInTheDocument();
  });

  it('updates only the value and clears the field even when saving fails', async () => {
    const puts = fakeApi(400);
    renderWithProviders(<SecretsPage owner="team:acme/dev" onOwnerChange={() => {}} />);
    await userEvent.click(await screen.findByRole('button', { name: 'Change npm_token' }));
    await userEvent.click(await screen.findByRole('menuitem', { name: 'Update value' }));
    const dialog = await screen.findByRole('dialog');
    const value = within(dialog).getByLabelText('Value');
    await userEvent.type(value, 'npm_real');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Save' }));
    await expect
      .poll(() => puts)
      .toEqual([{ name: 'npm_token', owner: 'acme/dev', body: { value: 'npm_real' } }]);
    await expect.poll(() => (value as HTMLInputElement).value).toBe('');
  });

  it('registers a personal secret from a template', async () => {
    const puts = fakeApi();
    renderWithProviders(<SecretsPage onOwnerChange={() => {}} />);
    expect(await screen.findByText('No secrets yet.')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: /New secret/ }));
    await userEvent.click(await screen.findByRole('menuitem', { name: 'anthropic_api_key' }));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByRole('textbox', { name: 'Name' })).toHaveValue('anthropic_api_key');
    await userEvent.type(within(dialog).getByLabelText('Value of value'), 'sk-ant-real');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Save' }));
    await expect
      .poll(() => puts)
      .toEqual([
        {
          name: 'anthropic_api_key',
          owner: null,
          body: {
            env: ['ANTHROPIC_API_KEY'],
            placeholder: 'sk-ant-api03-{id}',
            value: 'sk-ant-real',
            inject: [
              {
                field: 'value',
                hosts: ['api.anthropic.com'],
                header: 'x-api-key',
                format: 'replace',
              },
            ],
            sign: [],
            files: [],
          },
        },
      ]);
    // 保存すると閉じる。
    await expect.poll(() => screen.queryByRole('dialog')).toBeNull();
  });
});
