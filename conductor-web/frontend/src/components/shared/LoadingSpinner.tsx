import { useState, useEffect } from "react";

const messages = [
  "Checking the timetable\u2026",
  "Pulling into the station\u2026",
  "Consulting the signal box\u2026",
  "Stoking the engine\u2026",
  "Reading the departure board\u2026",
];

export function LoadingSpinner() {
  const [index, setIndex] = useState(() => Math.floor(Math.random() * messages.length));

  useEffect(() => {
    const id = setInterval(() => {
      setIndex((i) => (i + 1) % messages.length);
    }, 2000);
    return () => clearInterval(id);
  }, []);

  return (
    <div className="flex flex-col items-center justify-center py-12 gap-3">
      <div className="h-6 w-6 animate-spin rounded-full border-2 border-gray-300 border-t-indigo-600" />
      <span className="text-xs text-gray-500">{messages[index]}</span>
    </div>
  );
}
