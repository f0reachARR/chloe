import { Alert, Button, Center, Code, Paper, Stack, Text, Title } from '@mantine/core';
import { IconBrandGithub } from '@tabler/icons-react';
import { useSearchParams } from 'react-router';

import { api } from '../api/client';

export function Login() {
  const [params] = useSearchParams();
  const error = params.get('error');
  const { data: methods, isPending } = api.useQuery('get', '/api/auth/methods');

  return (
    <Center mih="100dvh" p="md">
      <Paper withBorder shadow="sm" p="xl" w="100%" maw={420}>
        <Stack>
          <Title order={2}>chloe</Title>
          {error && (
            <Alert color="red" title="Could not sign in">
              {error}
            </Alert>
          )}
          {methods?.github && (
            <Button
              component="a"
              href="/api/auth/github"
              size="md"
              leftSection={<IconBrandGithub size={20} />}
              fullWidth
            >
              Sign in with GitHub
            </Button>
          )}
          {!isPending && (
            <Text size="sm" c="dimmed">
              {methods?.github ? 'Or run ' : 'Run '}
              <Code>chloe web login</Code> over SSH and open the URL it shows (or scan its QR
              code).
            </Text>
          )}
        </Stack>
      </Paper>
    </Center>
  );
}
