import { Select, type SelectProps } from '@mantine/core';

import { type OwnerEntry, ownerGroups } from '@/shared/owners';

// 持ち主 (`user:alice`、`org:acme`、`team:acme/dev`) を選ぶ欄。
export function OwnerSelect({
  owners,
  ...props
}: { owners: OwnerEntry[] } & Omit<SelectProps, 'data'>) {
  return (
    <Select
      allowDeselect={false}
      searchable={owners.length > 8}
      data={ownerGroups(owners)}
      {...props}
    />
  );
}
