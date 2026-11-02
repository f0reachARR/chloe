import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import { http, server } from '@/test/msw';
import { renderWithProviders } from '@/test/render';

import { DeleteSection } from './DeleteSection';

describe('DeleteSection', () => {
  it('deletes only after the name is typed, with the disks if asked', async () => {
    const calls: string[] = [];
    server.use(
      http.delete('/api/v1/workspaces/{name}', ({ params, query, response }) => {
        calls.push(`${params.name}?disks=${query.get('disks')}`);
        return response(204).empty();
      }),
    );
    renderWithProviders(<DeleteSection workspace="acme/web" onDirtyChange={() => {}} />);
    const button = await screen.findByRole('button', { name: 'Delete workspace' });
    expect(button).toBeDisabled();
    await userEvent.type(screen.getByRole('textbox', { name: /to confirm/ }), 'acme/we');
    expect(button).toBeDisabled();
    await userEvent.type(screen.getByRole('textbox', { name: /to confirm/ }), 'b');
    await userEvent.click(screen.getByRole('checkbox', { name: /Also delete the disks/ }));
    await userEvent.click(button);
    await expect.poll(() => calls).toEqual(['acme/web?disks=true']);
  });
});
