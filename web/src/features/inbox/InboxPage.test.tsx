import { screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { networkAsk, questionAsk } from '@/test/fixtures';
import { http, server } from '@/test/msw';
import { renderWithProviders } from '@/test/render';

import { InboxPage } from './InboxPage';

describe('InboxPage', () => {
  it('lists pending asks', async () => {
    server.use(
      http.get('/api/v1/asks', ({ query, response }) =>
        response(200).json(query.get('all') ? [] : [questionAsk(), networkAsk()]),
      ),
    );
    renderWithProviders(<InboxPage all={false} onAllChange={() => {}} />);
    expect(await screen.findByText('Which branch?')).toBeInTheDocument();
    expect(screen.getByText('docs.example.com')).toBeInTheDocument();
  });

  it('says when nothing is waiting', async () => {
    server.use(http.get('/api/v1/asks', ({ response }) => response(200).json([])));
    renderWithProviders(<InboxPage all={false} onAllChange={() => {}} />);
    expect(await screen.findByText('No asks are waiting for an answer.')).toBeInTheDocument();
  });

  it('shows API errors', async () => {
    server.use(
      http.get('/api/v1/asks', ({ response }) =>
        response('4XX').json({ error: 'no access' }, { status: 403 }),
      ),
    );
    renderWithProviders(<InboxPage all={false} onAllChange={() => {}} />);
    expect(await screen.findByText('no access')).toBeInTheDocument();
  });
});
