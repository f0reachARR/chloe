// スマートフォンのキーボードにないキー (端末の下の列に並べる)。
export const extraKeys = [
  { label: 'Esc', data: '\x1b' },
  { label: 'Tab', data: '\t' },
  { label: '↑', data: '\x1b[A' },
  { label: '↓', data: '\x1b[B' },
  { label: '←', data: '\x1b[D' },
  { label: '→', data: '\x1b[C' },
] as const;

// Ctrl を押したまま打った一文字にする (`c` → ^C)。Ctrl と組める文字でなければ、そのまま返す。
export function withCtrl(data: string): string {
  if (data.length !== 1) {
    return data;
  }
  const upper = data.toUpperCase();
  const code = upper.charCodeAt(0);
  // @ A-Z [ \ ] ^ _ (0x40-0x5f) は 0x00-0x1f に、? は DEL になる。
  if (code >= 0x40 && code <= 0x5f) {
    return String.fromCharCode(code & 0x1f);
  }
  if (upper === '?') {
    return '\x7f';
  }
  if (upper === ' ') {
    return '\x00';
  }
  return data;
}
