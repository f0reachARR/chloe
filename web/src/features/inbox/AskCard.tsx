import {
  Badge,
  Button,
  Card,
  Code,
  Group,
  Stack,
  Text,
  TextInput,
  Textarea,
  Tooltip,
} from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconCheck, IconMessageQuestion, IconNetwork, IconX } from '@tabler/icons-react';
import { useQueryClient } from '@tanstack/react-query';
import { useState } from 'react';

import { type Schemas, api, errorMessage } from '@/shared/api/client';
import { absoluteTime, relativeTime } from '@/shared/lib/time';

import { reloadAsks } from './api';

type AskInfo = Schemas['AskInfo'];
type Answer = Schemas['AnswerBody'];

const statusColor: Record<AskInfo['status'], string> = {
  pending: 'blue',
  allowed: 'teal',
  answered: 'teal',
  denied: 'red',
  expired: 'gray',
  cancelled: 'gray',
};

// エージェントからの依頼と、それに答える操作。依頼の文は VM の中から来るので、文字列のまま
// 表示する (HTML として解釈しない)。
export function AskCard({ ask }: { ask: AskInfo }) {
  const queryClient = useQueryClient();
  const answer = api.useMutation('post', '/api/v1/asks/{id}/answer', {
    onSuccess: (data) => {
      notifications.show({ color: 'teal', message: `Ask ${data.id} is ${data.status}` });
    },
    onError: (error) => {
      notifications.show({ color: 'red', title: 'Could not answer', message: errorMessage(error) });
    },
    onSettled: () => reloadAsks(queryClient),
  });
  const [denying, setDenying] = useState(false);
  const [message, setMessage] = useState('');
  const [text, setText] = useState('');

  const send = (body: Answer) => answer.mutate({ params: { path: { id: ask.id } }, body });
  const pending = ask.status === 'pending';
  const busy = answer.isPending;

  return (
    <Card withBorder padding="md">
      <Stack gap="sm">
        <Group justify="space-between" wrap="nowrap" align="flex-start">
          <Group gap="xs" wrap="nowrap" miw={0}>
            {ask.kind === 'network' ? (
              <Badge leftSection={<IconNetwork size={12} />} variant="light">
                Network
              </Badge>
            ) : (
              <Badge leftSection={<IconMessageQuestion size={12} />} variant="light" color="grape">
                Question
              </Badge>
            )}
            <Text size="sm" ff="monospace" truncate>
              {ask.workspace}
            </Text>
          </Group>
          <Tooltip label={absoluteTime(ask.created_at)}>
            <Text size="xs" c="dimmed" style={{ whiteSpace: 'nowrap' }}>
              {relativeTime(ask.created_at)}
            </Text>
          </Tooltip>
        </Group>

        {ask.kind === 'network' ? (
          <Stack gap={4}>
            <Text fw={600} ff="monospace" style={{ wordBreak: 'break-all' }}>
              {ask.host}
            </Text>
            {(ask.methods?.length || ask.paths?.length) && (
              <Group gap={4}>
                {ask.methods?.map((m) => (
                  <Badge key={m} size="sm" variant="outline">
                    {m}
                  </Badge>
                ))}
                {ask.paths?.map((p) => (
                  <Code key={p}>{p}</Code>
                ))}
              </Group>
            )}
          </Stack>
        ) : (
          <Text fw={500} style={{ whiteSpace: 'pre-wrap' }}>
            {ask.question}
          </Text>
        )}

        {ask.reason && (
          <Text size="sm" c="dimmed" style={{ whiteSpace: 'pre-wrap' }}>
            {ask.reason}
          </Text>
        )}

        {pending ? (
          <>
            {ask.kind === 'network' ? (
              <Stack gap="xs">
                <Group grow>
                  {ask.can_allow && (
                    <Button
                      leftSection={<IconCheck size={16} />}
                      loading={busy}
                      onClick={() => send({ allow: true })}
                    >
                      Allow
                    </Button>
                  )}
                  <Button
                    variant="light"
                    color="red"
                    leftSection={<IconX size={16} />}
                    disabled={busy}
                    onClick={() => setDenying((d) => !d)}
                  >
                    Deny
                  </Button>
                </Group>
                {!ask.can_allow && (
                  <Text size="xs" c="dimmed">
                    Only people who can change this workspace's rules can allow it.
                  </Text>
                )}
              </Stack>
            ) : (
              <Stack gap="xs">
                {ask.options && ask.options.length > 0 && (
                  <Stack gap={6}>
                    {ask.options.map((option) => (
                      <Button
                        key={option}
                        variant="light"
                        justify="flex-start"
                        disabled={busy}
                        onClick={() => send({ answer: option })}
                        styles={{ label: { whiteSpace: 'normal', textAlign: 'left' } }}
                        h="auto"
                        py={8}
                      >
                        {option}
                      </Button>
                    ))}
                  </Stack>
                )}
                <Group gap="xs" wrap="nowrap" align="flex-start">
                  <TextInput
                    flex={1}
                    placeholder={ask.options?.length ? 'Or write an answer' : 'Your answer'}
                    value={text}
                    onChange={(event) => setText(event.currentTarget.value)}
                    onKeyDown={(event) => {
                      if (event.key === 'Enter' && text.trim()) {
                        send({ answer: text });
                      }
                    }}
                  />
                  <Button
                    disabled={!text.trim()}
                    loading={busy}
                    onClick={() => send({ answer: text })}
                  >
                    Send
                  </Button>
                </Group>
                <Button
                  variant="subtle"
                  color="red"
                  size="compact-sm"
                  disabled={busy}
                  onClick={() => setDenying((d) => !d)}
                  style={{ alignSelf: 'flex-start' }}
                >
                  Decline to answer
                </Button>
              </Stack>
            )}
            {denying && (
              <Stack gap="xs">
                <Textarea
                  placeholder="Tell the agent why (optional)"
                  autosize
                  minRows={2}
                  value={message}
                  onChange={(event) => setMessage(event.currentTarget.value)}
                />
                <Button
                  color="red"
                  loading={busy}
                  onClick={() => send({ deny: true, message: message.trim() || null })}
                >
                  {ask.kind === 'network' ? 'Deny access' : 'Decline'}
                </Button>
              </Stack>
            )}
            <Text size="xs" c="dimmed">
              Expires {relativeTime(ask.expires_at)}
            </Text>
          </>
        ) : (
          <Stack gap={4}>
            <Group gap="xs">
              <Badge color={statusColor[ask.status]} variant="light">
                {ask.status}
              </Badge>
              {ask.answered_by && (
                <Text size="xs" c="dimmed">
                  by {ask.answered_by}
                  {ask.answered_at ? `, ${relativeTime(ask.answered_at)}` : ''}
                </Text>
              )}
            </Group>
            {ask.answer && (
              <Text size="sm" style={{ whiteSpace: 'pre-wrap' }}>
                {ask.answer}
              </Text>
            )}
          </Stack>
        )}
      </Stack>
    </Card>
  );
}
