const UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
  ["day", 86400000],
  ["hour", 3600000],
  ["minute", 60000],
  ["second", 1000],
];

const rtf = new Intl.RelativeTimeFormat("en", { numeric: "auto" });

export function TimeAgo({ date }: { date: string }) {
  const diff = new Date(date).getTime() - Date.now();
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
