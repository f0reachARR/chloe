import { Alert, Button, Code, Text } from '@mantine/core';
import { IconBrandGithub } from '@tabler/icons-react';
import { useQuery } from '@tanstack/react-query';

import { methodsQuery } from './api';
import { AuthCard } from './AuthCard';

export function LoginPage({ error }: { error?: string }) {
  const { data: methods, isPending } = useQuery(methodsQuery());
  return (
    <AuthCard>
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
          <Code>chloe web login</Code> over SSH and open the URL it shows (or scan its QR code).
        </Text>
      )}
    </AuthCard>
  );
}
