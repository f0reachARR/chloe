import { Alert, Button, Code, Text } from '@mantine/core';
import { useState } from 'react';

import { errorMessage, fetchClient } from '@/shared/api/client';

import { AuthCard } from './AuthCard';

// `chloe web login` の URL を開いた画面。コードは、ボタンを押してから使う (リンクのプレビューを
// 作るボットが開いただけで、一度だけのコードを使い切らないように)。
export function LinkPage({ code }: { code?: string }) {
  const [error, setError] = useState<string>();
  const [pending, setPending] = useState(false);

  const signIn = async () => {
    if (!code) return;
    setPending(true);
    setError(undefined);
    const result = await fetchClient.POST('/api/auth/link', { body: { code } });
    if (result.error) {
      setError(errorMessage(result.error));
      setPending(false);
      return;
    }
    // ログインの前に公開したポートを開こうとしていれば、ゲートウェイがそこへ戻す。
    window.location.assign('/api/auth/continue');
  };

  return (
    <AuthCard>
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
    </AuthCard>
  );
}
