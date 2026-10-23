import { Badge } from '@mantine/core';
import { useQuery } from '@tanstack/react-query';

import { asksQuery } from './api';

// 答えを待つ依頼の数。
export function PendingBadge() {
  const count = useQuery(asksQuery()).data?.length ?? 0;
  return count > 0 ? (
    <Badge size="sm" circle>
      {count}
    </Badge>
  ) : null;
}
