// Policy のセクションのフォームの値と、管理 API の規則とのあいだの変換。
import type { Schemas } from '@/shared/api/client';

type PathRuleConfig = Schemas['PathRuleConfig'];

// ワークスペースの規則で使える action。中継のみ (passthrough) はゲートウェイの設定にしか置けない。
export type EditableAction = 'allow' | 'deny';

export interface RuleValues {
  // React の key (並べ替えても欄の状態がずれないように)。送らない。
  key: string;
  name: string;
  hosts: string[];
  methods: string[];
  paths: string[];
  action: EditableAction;
}

export interface PolicyValues {
  rules: RuleValues[];
}

let nextKey = 0;
const newKey = () => `rule-${nextKey++}`;

export const methodOptions = ['GET', 'HEAD', 'POST', 'PUT', 'PATCH', 'DELETE', 'OPTIONS'];

export function toRuleValues(rule: PathRuleConfig): RuleValues {
  return {
    key: newKey(),
    name: rule.name ?? '',
    hosts: rule.hosts,
    methods: rule.methods ?? [],
    paths: rule.paths ?? [],
    action: rule.action === 'deny' ? 'deny' : 'allow',
  };
}

export function emptyRule(): RuleValues {
  return { key: newKey(), name: '', hosts: [], methods: [], paths: [], action: 'allow' };
}

// 空の項目は送らない (メソッドとパスは、空ならすべてに一致する)。
export function toRuleConfig(values: RuleValues): PathRuleConfig {
  const name = values.name.trim();
  return {
    ...(name ? { name } : {}),
    hosts: values.hosts,
    ...(values.methods.length ? { methods: values.methods.map((m) => m.toUpperCase()) } : {}),
    ...(values.paths.length ? { paths: values.paths } : {}),
    action: values.action,
  };
}

export const toRuleConfigs = (values: PolicyValues): PathRuleConfig[] =>
  values.rules.map(toRuleConfig);

// 送る形が同じなら、変えていない。
export function sameRules(a: PathRuleConfig[], b: PathRuleConfig[]): boolean {
  return JSON.stringify(a.map(normalize)) === JSON.stringify(b.map(normalize));
}

function normalize(rule: PathRuleConfig) {
  return {
    name: rule.name ?? null,
    hosts: rule.hosts,
    methods: rule.methods ?? [],
    paths: rule.paths ?? [],
    action: rule.action,
  };
}

// 規則の出どころの印。
export function sourceLabel(source: Schemas['RuleSource']): string {
  switch (source.kind) {
    case 'gateway':
      return 'gateway';
    case 'owner':
      return source.owner;
    case 'workspace':
      return 'workspace';
    case 'secret':
      return 'secrets';
  }
}
