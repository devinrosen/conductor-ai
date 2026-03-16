const UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
  ["day", 86400000],
  ["hour", 3600000],
  ["minute", 60000],
  ["second", 1000],
];

const rtf = new Intl.RelativeTimeFormat("en", { numeric: "auto" });

const SHORT_UNITS: [number, string][] = [
  [86400000, "d"],
  [3600000, "h"],
  [60000, "m"],
  [1000, "s"],
];

export function TimeAgo({ date, short }: { date: string; short?: boolean }) {
  const diff = new Date(date).getTime() - Date.now();

  if (short) {
    const abs = Math.abs(diff);
    for (const [ms, suffix] of SHORT_UNITS) {
      if (abs >= ms || suffix === "s") {
        return (
          <time dateTime={date} title={date}>
            {Math.round(abs / ms)}{suffix}
          </time>
        );
      }
    }
    return <time dateTime={date}>0s</time>;
  }

  for (const [unit, ms] of UNITS) {
    if (Math.abs(diff) >= ms || unit === "second") {
      return (
        <time dateTime={date} title={date}>
          {rtf.format(Math.round(diff / ms), unit)}
        </time>
      );
    }
  }
  return <time dateTime={date}>just now</time>;
}
