import { TextInput } from '@mantine/core';
import { IconCpu, IconShield } from '@tabler/icons-react';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useEffect, useState } from 'react';
import { describe, expect, it, vi } from 'vitest';

import type { WorkspaceSettingsSection, WorkspaceSettingsSectionProps } from '@/shared/feature';
import { renderWithProviders } from '@/test/render';

import { WorkspaceSettings } from './WorkspaceSettings';

// 書き換えると変更ありと知らせるだけのセクション。
function FakeSection({ onDirtyChange }: WorkspaceSettingsSectionProps) {
  const [value, setValue] = useState('');
  useEffect(() => onDirtyChange(value !== ''), [value, onDirtyChange]);
  return (
    <TextInput label="Value" value={value} onChange={(e) => setValue(e.currentTarget.value)} />
  );
}

const sections: WorkspaceSettingsSection[] = [
  { id: 'machine', label: 'Machine', icon: IconCpu, order: 10, component: FakeSection },
  {
    id: 'policy',
    label: 'Policy',
    icon: IconShield,
    order: 30,
    component: () => <p>policy rules</p>,
  },
];

describe('WorkspaceSettings', () => {
  it('opens the section in the URL and keeps edits across tabs', async () => {
    const onSectionChange = vi.fn<(section: string) => void>();
    renderWithProviders(
      <WorkspaceSettings
        workspace="acme/proj"
        sections={sections}
        section="policy"
        onSectionChange={onSectionChange}
        onClose={() => {}}
      />,
    );
    expect(await screen.findByRole('tab', { name: 'Policy' })).toHaveAttribute(
      'aria-selected',
      'true',
    );
    await userEvent.click(screen.getByRole('tab', { name: 'Machine' }));
    expect(onSectionChange).toHaveBeenLastCalledWith('machine');
  });

  it('asks before discarding unsaved changes', async () => {
    const onClose = vi.fn<() => void>();
    renderWithProviders(
      <WorkspaceSettings
        workspace="acme/proj"
        sections={sections}
        onSectionChange={() => {}}
        onClose={onClose}
      />,
    );
    await userEvent.type(await screen.findByRole('textbox', { name: 'Value' }), 'x');
    await userEvent.keyboard('{Escape}');
    expect(screen.getByText('Discard unsaved changes?')).toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole('button', { name: 'Discard' }));
    expect(onClose).toHaveBeenCalled();
  });

  it('closes right away without changes', async () => {
    const onClose = vi.fn<() => void>();
    renderWithProviders(
      <WorkspaceSettings
        workspace="acme/proj"
        sections={sections}
        onSectionChange={() => {}}
        onClose={onClose}
      />,
    );
    await screen.findByRole('textbox', { name: 'Value' });
    await userEvent.keyboard('{Escape}');
    expect(onClose).toHaveBeenCalled();
  });
});
