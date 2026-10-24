import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { workspace } from '@/test/fixtures';
import { http, server } from '@/test/msw';
import { renderWithProviders } from '@/test/render';

import { MachineSection } from './MachineSection';

// 送った本文を残し、当てはめたワークスペースを返す偽の API。
function fakeWorkspace(initial = workspace({ name: 'acme/proj' })) {
  let current = initial;
  const bodies: unknown[] = [];
  server.use(
    http.get('/api/v1/workspaces/{name}', ({ params, response }) => {
      expect(params.name).toBe('acme/proj');
      return response(200).json(current);
    }),
    http.patch('/api/v1/workspaces/{name}', async ({ request, response }) => {
      const body = await request.json();
      bodies.push(body);
      current = {
        ...current,
        ...(body.vcpus != null && { vcpus: body.vcpus }),
        ...(body.running === false && { running: false }),
      };
      return response(200).json(current);
    }),
    http.get('/api/v1/workspaces', ({ response }) => response(200).json([current])),
  );
  return bodies;
}

describe('MachineSection', () => {
  it('saves only what changed and offers to stop to apply it', async () => {
    const bodies = fakeWorkspace();
    const onDirtyChange = vi.fn<(dirty: boolean) => void>();
    renderWithProviders(<MachineSection workspace="acme/proj" onDirtyChange={onDirtyChange} />);
    const vcpus = await screen.findByRole('textbox', { name: 'vCPUs' });
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled();

    await userEvent.clear(vcpus);
    await userEvent.type(vcpus, '8');
    expect(onDirtyChange).toHaveBeenLastCalledWith(true);
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    await expect.poll(() => bodies).toEqual([{ vcpus: 8 }]);
    expect(await screen.findByText('Restart to apply')).toBeInTheDocument();
    expect(onDirtyChange).toHaveBeenLastCalledWith(false);

    await userEvent.click(screen.getByRole('button', { name: 'Stop now' }));
    await expect.poll(() => bodies).toEqual([{ vcpus: 8 }, { running: false }]);
    expect(await screen.findByText('Stopped')).toBeInTheDocument();
  });

  it('does not let disks shrink', async () => {
    const bodies = fakeWorkspace();
    renderWithProviders(<MachineSection workspace="acme/proj" onDirtyChange={() => {}} />);
    const disk = await screen.findByRole('textbox', { name: 'Docker disk (GiB)' });
    await userEvent.clear(disk);
    await userEvent.type(disk, '10');
    await userEvent.tab();
    // 今の大きさより小さい値は、欄を離れると今の大きさに戻る。
    expect(disk).toHaveValue('40');
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled();
    expect(bodies).toEqual([]);
  });

  it('asks before stopping', async () => {
    const bodies = fakeWorkspace();
    renderWithProviders(<MachineSection workspace="acme/proj" onDirtyChange={() => {}} />);
    await userEvent.click(await screen.findByRole('button', { name: 'Stop' }));
    expect(screen.getByText('Stop this workspace?')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(bodies).toEqual([]);
    await userEvent.click(screen.getByRole('button', { name: 'Stop' }));
    await userEvent.click(screen.getByRole('button', { name: 'Stop now' }));
    await expect.poll(() => bodies).toEqual([{ running: false }]);
  });
});
