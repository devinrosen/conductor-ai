const colors: Record<string, string> = {
  active: "bg-green-100 text-green-700",
  merged: "bg-blue-100 text-blue-700",
  abandoned: "bg-gray-100 text-gray-600",
  open: "bg-green-100 text-green-700",
  closed: "bg-red-100 text-red-700",
};

export function StatusBadge({ status }: { status: string }) {
  const color = colors[status] ?? "bg-gray-100 text-gray-600";
  return (
    <span className={`inline-block px-2 py-0.5 text-xs font-medium rounded-full ${color}`}>
      {status}
    </span>
  );
}
