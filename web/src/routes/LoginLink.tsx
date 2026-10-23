import { Alert, Button, Center, Code, Paper, Stack, Text, Title } from '@mantine/core';
import { useState } from 'react';
import { useSearchParams } from 'react-router';

import { errorMessage, fetchClient } from '../api/client';

// `chloe web login` の URL を開いた画面。コードは、ボタンを押してから使う (リンクのプレビューを
// 作るボットが開いただけで、一度だけのコードを使い切らないように)。
export function LoginLink() {
  const [params] = useSearchParams();
  const code = params.get('code') ?? '';
  const [error, setError] = useState<string>();
  const [pending, setPending] = useState(false);

  const signIn = async () => {
    setPending(true);
    setError(undefined);
    const { error } = await fetchClient.POST('/api/auth/link', { body: { code } });
    if (error) {
      setError(errorMessage(error));
      setPending(false);
      return;
    }
    window.location.assign('/');
  };

  return (
    <Center mih="100dvh" p="md">
      <Paper withBorder shadow="sm" p="xl" w="100%" maw={420}>
        <Stack>
          <Title order={2}>chloe</Title>
          {code ? (
            <Text>
              Sign in with the link from <Code>chloe web login</Code>.
            </Text>
          ) : (
            <Alert color="red">This link has no code. Run chloe web login again.</Alert>
          )}
          {error && (
            <Alert color="red" title="Could not sign in">
              {error}
            </Alert>
          )}
          <Button size="md" fullWidth disabled={!code} loading={pending} onClick={signIn}>
            Sign in
          </Button>
        </Stack>
      </Paper>
    </Center>
  );
}
