import { Link } from "react-router";

interface BreadcrumbStop {
  label: string;
  href?: string;
  current?: boolean;
}

export function TransitBreadcrumb({ stops }: { stops: BreadcrumbStop[] }) {
  return (
    <nav className="flex items-center gap-0 text-xs" aria-label="Breadcrumb">
      {stops.map((stop, i) => (
        <div key={i} className="flex items-center">
          {i > 0 && (
            <div className="w-8 h-px mx-1" style={{ backgroundColor: "var(--color-gray-300)" }} />
          )}
          <div className="flex items-center gap-1.5">
            <div
              className="shrink-0"
              style={{
                width: stop.current ? 8 : 6,
                height: stop.current ? 8 : 6,
                borderRadius: "50%",
                backgroundColor: stop.current ? "var(--color-indigo-500)" : "transparent",
                border: stop.current ? "none" : "1.5px solid var(--color-gray-400)",
              }}
            />
            {stop.current ? (
              <span className="font-medium text-gray-800">{stop.label}</span>
            ) : stop.href ? (
              <Link to={stop.href} className="text-gray-500 hover:text-indigo-600">{stop.label}</Link>
            ) : (
              <span className="text-gray-400">{stop.label}</span>
            )}
          </div>
        </div>
      ))}
    </nav>
  );
}
