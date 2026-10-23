// 作業中の印 (`<ツール>-<セッションの ID>`) を、ツールごとの数にまとめる (`claude ×2, codex`)。
export function agents(busy: string[]): string {
  const counts = new Map<string, number>();
  for (const mark of busy) {
    const tool = mark.split('-')[0] || mark;
    counts.set(tool, (counts.get(tool) ?? 0) + 1);
  }
  return [...counts].map(([tool, n]) => (n > 1 ? `${tool} ×${n}` : tool)).join(', ');
}
