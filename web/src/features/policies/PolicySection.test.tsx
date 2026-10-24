import { screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import type { Schemas } from '@/shared/api/client';
import { http, server } from '@/test/msw';
import { renderWithProviders } from '@/test/render';

import { PolicySection } from './PolicySection';

type WorkspaceRules = Schemas['WorkspaceRules'];
type PathRuleConfig = Schemas['PathRuleConfig'];

const gatewayRule: PathRuleConfig = { hosts: ['github.com'], action: 'allow' };

// 規則を置き換えるたびに版が変わる偽の API。`conflictOnce` なら、最初の置き換えを 412 で断る
// (編集しているあいだに承認で規則が足された、とみなす)。
function fakeRules({ own = [] as PathRuleConfig[], editable = true, conflictOnce = false } = {}) {
  let current = own;
  let version = 'v1';
  const puts: { ifMatch: string | null; body: unknown }[] = [];
  const rules = (): WorkspaceRules => ({
    rules: [
      { source: { kind: 'gateway' }, rule: gatewayRule },
      ...current.map((rule) => ({ source: { kind: 'workspace' as const }, rule })),
    ],
    own: current,
    editable,
    version,
  });
  server.use(
    http.get('/api/v1/workspace-rules/{name}', ({ params, response }) => {
      expect(params.name).toBe('acme/proj');
      return response(200).json(rules());
    }),
    http.put('/api/v1/workspace-policies/{name}', async ({ request, response }) => {
      const body = await request.json();
      const ifMatch = request.headers.get('If-Match');
      puts.push({ ifMatch, body });
      if (conflictOnce) {
        conflictOnce = false;
        current = [...current, { name: 'ask-9', hosts: ['docs.example.com'], action: 'allow' }];
        version = 'v2';
        return response(412).json({ error: 'the rules changed' });
      }
      if (ifMatch !== version) return response(412).json({ error: 'the rules changed' });
      current = body.rules ?? [];
      version = `v${Number(version.slice(1)) + 1}`;
      return response(200).json({ updated_runners: 1 });
    }),
  );
  return puts;
}

async function addHost(host: string) {
  await userEvent.click(screen.getByRole('button', { name: 'Add rule' }));
  const inputs = screen.getAllByRole('combobox', { name: /^Hosts of rule/ });
  await userEvent.type(inputs[inputs.length - 1], `${host}{Enter}`);
}

describe('PolicySection', () => {
  it('adds a rule and saves it with the version it read', async () => {
    const puts = fakeRules();
    const onDirtyChange = vi.fn<(dirty: boolean) => void>();
    renderWithProviders(<PolicySection workspace="acme/proj" onDirtyChange={onDirtyChange} />);
    expect(await screen.findByText('This workspace adds no rules of its own.')).toBeInTheDocument();
    await addHost('staging.example.com');
    expect(onDirtyChange).toHaveBeenLastCalledWith(true);
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    await expect
      .poll(() => puts)
      .toEqual([
        { ifMatch: 'v1', body: { rules: [{ hosts: ['staging.example.com'], action: 'allow' }] } },
      ]);
    await expect.poll(() => onDirtyChange.mock.lastCall).toEqual([false]);
  });

  it('does not save a rule without hosts', async () => {
    const puts = fakeRules();
    renderWithProviders(<PolicySection workspace="acme/proj" onDirtyChange={() => {}} />);
    await userEvent.click(await screen.findByRole('button', { name: 'Add rule' }));
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(await screen.findByText('Add at least one host')).toBeInTheDocument();
    expect(puts).toEqual([]);
  });

  it('reorders rules', async () => {
    const puts = fakeRules({
      own: [
        { hosts: ['a.example'], action: 'allow' },
        { hosts: ['b.example'], action: 'deny' },
      ],
    });
    renderWithProviders(<PolicySection workspace="acme/proj" onDirtyChange={() => {}} />);
    await userEvent.click(await screen.findByRole('button', { name: 'Move rule 2 up' }));
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    await expect
      .poll(() => puts.map((p) => p.body))
      .toEqual([
        {
          rules: [
            { hosts: ['b.example'], action: 'deny' },
            { hosts: ['a.example'], action: 'allow' },
          ],
        },
      ]);
  });

  it('asks what to do when the rules changed while editing', async () => {
    const puts = fakeRules({ conflictOnce: true });
    renderWithProviders(<PolicySection workspace="acme/proj" onDirtyChange={() => {}} />);
    await screen.findByText('This workspace adds no rules of its own.');
    await addHost('staging.example.com');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    const alert = await screen.findByText('The rules changed while you were editing');
    // 編集した内容は残す。
    expect(screen.getByText('staging.example.com')).toBeInTheDocument();
    await userEvent.click(
      within(alert.closest('[role="alert"]') as HTMLElement).getByRole('button', {
        name: 'Save mine',
      }),
    );
    await expect.poll(() => puts.map((p) => p.ifMatch)).toEqual(['v1', 'v2']);
  });

  it('shows the rules of other layers and read-only rules', async () => {
    fakeRules({ own: [{ hosts: ['b.example'], action: 'deny' }], editable: false });
    renderWithProviders(<PolicySection workspace="acme/proj" onDirtyChange={() => {}} />);
    expect(await screen.findByText(/You can view but not change/)).toBeInTheDocument();
    expect(screen.getByText('b.example')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Add rule' })).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: /Rules from other layers \(1\)/ }));
    expect(screen.getByText('github.com')).toBeInTheDocument();
    expect(screen.getByText('gateway')).toBeInTheDocument();
  });
});
