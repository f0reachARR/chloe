// 個人、Organization、team の設定 (PolicyLayer) と、フォームの値とのあいだの変換。
import type { Schemas } from '@/shared/api/client';

import { type RuleValues, sameRules, toRuleConfig, toRuleValues } from './rules';

type PolicyLayer = Schemas['PolicyLayer'];

export const kindLabel: Record<Schemas['ScopeKind'], string> = {
  gateway: 'Gateway',
  user: 'Personal',
  org: 'Organization',
  team: 'Team',
};

export interface PermissionValues {
  // React の key。送らない。
  key: string;
  name: string;
  level: string;
}

export interface LayerValues {
  rules: RuleValues[];
  // 新しいワークスペースのルートの扱いの既定。`inherit` なら書かない (上の層か既定に従う)。
  root: 'inherit' | Schemas['RootMode'];
  // インストールトークンの範囲。`inherit` なら書かない。
  github: 'inherit' | 'set';
  // `all` なら書かない (インストールのすべて)。
  repositories: 'all' | 'listed';
  repositoryList: string[];
  permissions: 'all' | 'listed';
  permissionList: PermissionValues[];
}

// GitHub の権限の名前の候補 (ほかの名前も書ける)。
export const permissionNames = [
  'actions',
  'checks',
  'contents',
  'deployments',
  'issues',
  'metadata',
  'packages',
  'pages',
  'pull_requests',
  'statuses',
  'workflows',
];

export const permissionLevels = ['read', 'write', 'admin'];

let nextKey = 0;
export const emptyPermission = (): PermissionValues => ({
  key: `permission-${nextKey++}`,
  name: '',
  level: 'read',
});

export function toLayerValues(layer: PolicyLayer): LayerValues {
  const github = layer.github;
  return {
    rules: (layer.rules ?? []).map(toRuleValues),
    root: layer.root ?? 'inherit',
    github: github ? 'set' : 'inherit',
    repositories: github?.repositories ? 'listed' : 'all',
    repositoryList: github?.repositories ?? [],
    permissions: github?.permissions ? 'listed' : 'all',
    permissionList: Object.entries(github?.permissions ?? {}).map(([name, level]) => ({
      ...emptyPermission(),
      name,
      level,
    })),
  };
}

// 書かない項目は送らない (個人の設定には `github` を書けないので、`personal` なら送らない)。
export function toLayer(values: LayerValues, personal: boolean): PolicyLayer {
  const layer: PolicyLayer = {};
  if (values.rules.length) layer.rules = values.rules.map(toRuleConfig);
  if (values.root !== 'inherit') layer.root = values.root;
  if (!personal && values.github === 'set') {
    layer.github = {
      ...(values.repositories === 'listed' ? { repositories: values.repositoryList } : {}),
      ...(values.permissions === 'listed'
        ? {
            permissions: Object.fromEntries(
              values.permissionList.map((p) => [p.name.trim(), p.level]),
            ),
          }
        : {}),
    };
  }
  return layer;
}

// 送る形が同じなら、変えていない。
export function sameLayer(a: PolicyLayer, b: PolicyLayer): boolean {
  return (
    sameRules(a.rules ?? [], b.rules ?? []) &&
    (a.root ?? null) === (b.root ?? null) &&
    JSON.stringify(normalizeGitHub(a.github)) === JSON.stringify(normalizeGitHub(b.github))
  );
}

function normalizeGitHub(github: PolicyLayer['github']) {
  if (!github) return null;
  return {
    repositories: github.repositories ?? null,
    permissions: github.permissions
      ? Object.fromEntries(
          Object.entries(github.permissions).toSorted(([a], [b]) => a.localeCompare(b)),
        )
      : null,
  };
}
