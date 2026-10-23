import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import { networkAsk, questionAsk } from '@/test/fixtures';
import { http, server } from '@/test/msw';
import { renderWithProviders } from '@/test/render';

import { AskCard } from './AskCard';

// 答えの本文を受け取り、答えた依頼を返す偽の API。
function captureAnswers() {
  const bodies: unknown[] = [];
  server.use(
    http.post('/api/v1/asks/{id}/answer', async ({ request, params, response }) => {
      bodies.push(await request.json());
      return response(200).json({ ...networkAsk({ id: Number(params.id) }), status: 'allowed' });
    }),
    http.get('/api/v1/asks', ({ response }) => response(200).json([])),
  );
  return bodies;
}

describe('AskCard', () => {
  it('allows a network ask', async () => {
    const bodies = captureAnswers();
    renderWithProviders(<AskCard ask={networkAsk()} />);
    expect(await screen.findByText('docs.example.com')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Allow' }));
    await expect.poll(() => bodies).toEqual([{ allow: true }]);
  });

  it('does not offer Allow to people who cannot change the rules', async () => {
    renderWithProviders(<AskCard ask={networkAsk({ can_allow: false })} />);
    expect(await screen.findByText(/Only people who can change/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Allow' })).not.toBeInTheDocument();
  });

  it('answers a question with an option or with text', async () => {
    const bodies = captureAnswers();
    renderWithProviders(<AskCard ask={questionAsk()} />);
    await userEvent.click(await screen.findByRole('button', { name: 'main' }));
    await userEvent.type(screen.getByPlaceholderText('Or write an answer'), 'dev please');
    await userEvent.click(screen.getByRole('button', { name: 'Send' }));
    await expect.poll(() => bodies).toEqual([{ answer: 'main' }, { answer: 'dev please' }]);
  });

  it('denies with a message for the agent', async () => {
    const bodies = captureAnswers();
    renderWithProviders(<AskCard ask={networkAsk()} />);
    await userEvent.click(await screen.findByRole('button', { name: 'Deny' }));
    await userEvent.type(screen.getByPlaceholderText(/Tell the agent why/), 'not now');
    await userEvent.click(screen.getByRole('button', { name: 'Deny access' }));
    await expect.poll(() => bodies).toEqual([{ deny: true, message: 'not now' }]);
  });

  it('shows the answer of a resolved ask', async () => {
    renderWithProviders(
      <AskCard ask={questionAsk({ status: 'answered', answer: 'main', answered_by: 'alice' })} />,
    );
    expect(await screen.findByText('answered')).toBeInTheDocument();
    expect(screen.getByText(/by alice/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Send' })).not.toBeInTheDocument();
  });
});
