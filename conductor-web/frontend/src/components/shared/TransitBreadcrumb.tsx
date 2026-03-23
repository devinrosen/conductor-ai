import { Link } from "react-router";

/**
 * Transit map breadcrumb navigation.
 *
 * Shows a horizontal line with station dots. Current station is
 * highlighted, previous stations are linked, future stations dimmed.
 */

interface BreadcrumbStop {
  label: string;
  href?: string;
  current?: boolean;
}

export function TransitBreadcrumb({ stops }: { stops: BreadcrumbStop[] }) {
  return (
    <nav className="flex items-center gap-0 text-xs mb-2" aria-label="Breadcrumb">
      {stops.map((stop, i) => (
        <div key={i} className="flex items-center">
          {i > 0 && (
            <div className="w-6 h-0.5 bg-gray-300" />
          )}
          <div className="flex flex-col items-center gap-0.5">
            <div
              className={`w-2.5 h-2.5 rounded-full border-2 ${
                stop.current
                  ? "bg-indigo-500 border-indigo-500"
                  : stop.href
                  ? "bg-transparent border-gray-400"
                  : "bg-transparent border-gray-300"
              }`}
            />
            {stop.current ? (
              <span className="font-medium text-gray-800 whitespace-nowrap">{stop.label}</span>
            ) : stop.href ? (
              <Link to={stop.href} className="text-gray-500 hover:text-indigo-600 whitespace-nowrap">
                {stop.label}
              </Link>
            ) : (
              <span className="text-gray-400 whitespace-nowrap">{stop.label}</span>
            )}
          </div>
        </div>
      ))}
    </nav>
  );
}
