import { Center, Paper, Stack, Title } from '@mantine/core';
import type { ReactNode } from 'react';

export function AuthCard({ children }: { children: ReactNode }) {
  return (
    <Center mih="100dvh" p="md">
      <Paper withBorder shadow="sm" p="xl" w="100%" maw={420}>
        <Stack>
          <Title order={2}>chloe</Title>
          {children}
        </Stack>
      </Paper>
    </Center>
  );
}
