import { Link } from "react-router";

export function NotFoundPage() {
  return (
    <div className="flex flex-col items-center justify-center py-24">
      <h2 className="text-2xl font-bold text-gray-900">404</h2>
      <p className="mt-2 text-gray-500">Page not found</p>
      <Link to="/" className="mt-4 text-indigo-600 hover:underline text-sm">
        Back to dashboard
      </Link>
    </div>
  );
}
