export function EmptyState({ message }: { message: string }) {
  return (
    <div className="flex items-center justify-center py-12 text-gray-400 text-sm">
      {message}
    </div>
  );
}
