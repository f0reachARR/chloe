// 画面に入れる機能。機能を足すときは、ここに一行足す (ナビゲーションと出来事の受け口は、
// 各機能が申告したものから組む)。ルートは src/routes にファイルを足す。
import { inboxFeature } from '@/features/inbox';
import { workspacesFeature } from '@/features/workspaces';
import type { Feature, NavItem } from '@/shared/feature';

export const features: Feature[] = [inboxFeature, workspacesFeature];

export const navItems: NavItem[] = features.flatMap((feature) => feature.nav ?? []);
