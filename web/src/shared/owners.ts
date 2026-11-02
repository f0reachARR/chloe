// 利用者が入れる持ち主 (個人、Organization、team)。ワークスペースを作る先と、シークレットの持ち主を
// 選ぶのに使う。
import { api, type Schemas } from './api/client';

export type OwnerEntry = Schemas['OwnerEntry'];

export const ownersQuery = () => api.queryOptions('get', '/api/v1/owners');

export const ownerKindLabel: Record<Schemas['OwnerKind'], string> = {
  user: 'Personal',
  org: 'Organization',
  team: 'Team',
};

export interface OwnerGroup {
  group: string;
  items: { value: string; label: string }[];
}

// 選択の欄の候補。個人をまとめ、Organization とその team を Organization ごとにまとめる。
export function ownerGroups(owners: OwnerEntry[]): OwnerGroup[] {
  const groups: OwnerGroup[] = [];
  for (const owner of owners) {
    const group = owner.kind === 'user' ? 'Personal' : owner.name.split('/')[0];
    let entry = groups.find((g) => g.group === group);
    if (!entry) {
      entry = { group, items: [] };
      groups.push(entry);
    }
    entry.items.push({
      value: owner.owner,
      label: owner.kind === 'team' ? `${owner.name} (team)` : owner.name,
    });
  }
  return groups;
}
