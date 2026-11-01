import { Code, Stack, Text } from '@mantine/core';

import type { WorkspaceSettingsSectionProps } from '@/shared/feature';

import { WorkspacePorts } from './WorkspacePorts';

// 公開したポートの一覧と取り消し。公開は、そのポートで何が動いているかを知っている VM の中から行う。
export function PortsSection({ workspace }: WorkspaceSettingsSectionProps) {
  return (
    <Stack>
      <Text size="sm" c="dimmed">
        Ports published inside the VM with <Code>chloe-ask port publish PORT</Code>. Each opens in a
        new tab after you sign in.
      </Text>
      <WorkspacePorts workspace={workspace} emptyText="No ports are published." />
    </Stack>
  );
}
