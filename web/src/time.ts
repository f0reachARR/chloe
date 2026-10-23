const units: [Intl.RelativeTimeFormatUnit, number][] = [
  ['day', 86400],
  ['hour', 3600],
  ['minute', 60],
];

const format = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });

// UNIX 時刻 (秒) を、今からの相対の時刻 (「3 分前」など) にする。
export function relativeTime(unixSecs: number, now = Date.now() / 1000): string {
  const diff = unixSecs - now;
  for (const [unit, secs] of units) {
    if (Math.abs(diff) >= secs) {
      return format.format(Math.round(diff / secs), unit);
    }
  }
  return format.format(Math.round(diff), 'second');
}

export function absoluteTime(unixSecs: number): string {
  return new Date(unixSecs * 1000).toLocaleString();
}
