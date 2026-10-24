// 画面に入れる機能。機能を足すときは、ここに一行足す (ナビゲーション、出来事の受け口、重ねて開くもの、
// ワークスペースの設定は、各機能が申告したものから組む)。ルートは src/routes にファイルを足す。
import { inboxFeature } from '@/features/inbox';
import { workspacesFeature } from '@/features/workspaces';
import type {
  Feature,
  NavItem,
  WorkspaceOverlayProps,
  WorkspaceSettingsSection,
} from '@/shared/feature';
import type { WorkspaceOverlay } from '@/shared/overlays';
import type { ComponentType } from 'react';

export const features: Feature[] = [inboxFeature, workspacesFeature];

export const navItems: NavItem[] = features.flatMap((feature) => feature.nav ?? []);

export const overlays = features.flatMap((feature) =>
  Object.entries(feature.overlays ?? {}).map(([param, component]) => ({
    param: param as Exclude<WorkspaceOverlay, 'settings'>,
    component: component as ComponentType<WorkspaceOverlayProps>,
  })),
);

export const workspaceSettings: WorkspaceSettingsSection[] = features
  .flatMap((feature) => feature.workspaceSettings ?? [])
  .sort((a, b) => a.order - b.order);
