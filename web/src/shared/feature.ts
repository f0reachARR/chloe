// 機能 (features/<機能>) が枠に申告するもの。枠はこれを集めて、ナビゲーションと出来事の受け口を組む。
import type { QueryClient } from '@tanstack/react-query';
import type { LinkProps } from '@tanstack/react-router';
import type { ComponentType } from 'react';

export interface FeatureContext {
  queryClient: QueryClient;
  navigate: (to: LinkProps['to']) => void;
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

export interface Feature {
  name: string;
  nav?: NavItem[];
  events?: Record<string, EventHandler>;
}
