import { Alert, Button, Group, Modal, Tabs, Text } from '@mantine/core';
import { useMediaQuery } from '@mantine/hooks';
import { useEffect, useState } from 'react';

import type { WorkspaceSettingsSection } from '@/shared/feature';

// ワークスペースの設定。各機能が申告したセクションをタブに並べる。タブを替えても、セクションは
// 描いたままにする (保存していない変更を失わないように)。
export function WorkspaceSettings({
  workspace,
  sections,
  section,
  onSectionChange,
  onClose,
}: {
  workspace: string;
  sections: WorkspaceSettingsSection[];
  section?: string;
  onSectionChange: (section: string) => void;
  onClose: () => void;
}) {
  const narrow = useMediaQuery('(max-width: 47.99em)') ?? true;
  const [dirty, setDirty] = useState<Record<string, boolean>>({});
  const [confirming, setConfirming] = useState(false);
  const active = sections.some((s) => s.id === section) ? section : sections[0]?.id;
  const unsaved = sections.filter((s) => dirty[s.id]);

  // 閉じようとしたときに保存の途中だったなら、保存が終わって変更がなくなったところで閉じる。
  useEffect(() => {
    if (confirming && unsaved.length === 0) onClose();
  }, [confirming, unsaved.length, onClose]);

  const close = () => {
    if (unsaved.length > 0) {
      setConfirming(true);
      return;
    }
    onClose();
  };

  return (
    <Modal
      opened
      onClose={close}
      fullScreen={narrow}
      size="lg"
      title={
        <Text fw={600} ff="monospace" truncate>
          {workspace}
        </Text>
      }
    >
      {confirming && unsaved.length > 0 && (
        <Alert color="yellow" mb="md" title="Discard unsaved changes?">
          <Text size="sm" mb="xs">
            {unsaved.map((s) => s.label).join(', ')} has changes that are not saved.
          </Text>
          <Group gap="xs">
            <Button size="compact-sm" color="red" onClick={onClose}>
              Discard
            </Button>
            <Button size="compact-sm" variant="default" onClick={() => setConfirming(false)}>
              Keep editing
            </Button>
          </Group>
        </Alert>
      )}
      <Tabs value={active ?? null} onChange={(value) => value && onSectionChange(value)}>
        <Tabs.List mb="md">
          {sections.map(({ id, label, icon: Icon }) => (
            <Tabs.Tab key={id} value={id} leftSection={<Icon size={16} />}>
              {label}
              {dirty[id] ? ' •' : ''}
            </Tabs.Tab>
          ))}
        </Tabs.List>
        {sections.map(({ id, component: Section }) => (
          <Tabs.Panel key={id} value={id}>
            <Section
              workspace={workspace}
              onDirtyChange={(value) =>
                setDirty((prev) => (prev[id] === value ? prev : { ...prev, [id]: value }))
              }
            />
          </Tabs.Panel>
        ))}
      </Tabs>
    </Modal>
  );
}
