// 機能 (features/<機能>) が枠に申告するもの。枠はこれを集めて、ナビゲーション、出来事の受け口、
// 重ねて開くもの、ワークスペースの設定を組む。
import type { QueryClient } from '@tanstack/react-query';
import type { LinkProps, NavigateOptions } from '@tanstack/react-router';
import type { ComponentType } from 'react';

import type { WorkspaceOverlay } from './overlays';

export interface FeatureContext {
  queryClient: QueryClient;
  navigate: (options: NavigateOptions) => void;
}

// 出来事 (`/api/v1/events` の名前) ごとの処理。`resync` は、つながったときと取りこぼしたときに
// 呼ぶので、出来事を見ずに読み直す。
export type EventHandler = (data: unknown, context: FeatureContext) => void;

export interface NavItem {
  to: LinkProps['to'];
  label: string;
  icon: ComponentType<{ size?: number }>;
  // 項目の横に出す印 (答えを待つ依頼の数など)。出すものがなければ null を描く。
  badge?: ComponentType;
}

// 重ねて開くもの。開いているあいだだけ描かれる。
export interface WorkspaceOverlayProps {
  workspace: string;
  onClose: () => void;
}

// ワークスペースの設定のセクション。セクションは自分で読み、自分の保存のボタンで保存する
// (保存先の API がセクションごとに違うため)。保存していない変更があるかを `onDirtyChange` で
// 知らせると、閉じるときに枠が確かめる。
export interface WorkspaceSettingsSectionProps {
  workspace: string;
  onDirtyChange: (dirty: boolean) => void;
}

export interface WorkspaceSettingsSection {
  // URL の `section=` に置く。
  id: string;
  label: string;
  icon: ComponentType<{ size?: number }>;
  // 小さいほど前に並ぶ。
  order: number;
  component: ComponentType<WorkspaceSettingsSectionProps>;
}

export interface Feature {
  name: string;
  nav?: NavItem[];
  events?: Record<string, EventHandler>;
  // 設定は枠が描くので、ここには申告しない。
  overlays?: Partial<
    Record<Exclude<WorkspaceOverlay, 'settings'>, ComponentType<WorkspaceOverlayProps>>
  >;
  workspaceSettings?: WorkspaceSettingsSection[];
}
