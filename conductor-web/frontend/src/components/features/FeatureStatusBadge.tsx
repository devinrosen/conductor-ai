import type { FeatureStatus } from "../../api/types";

const statusConfig: Record<FeatureStatus, { label: string; className: string }> = {
  InProgress: {
    label: "In Progress",
    className: "bg-yellow-100 text-yellow-800",
  },
  ReadyForReview: {
    label: "Ready for Review",
    className: "bg-blue-100 text-blue-800",
  },
  Approved: {
    label: "Approved",
    className: "bg-green-100 text-green-800",
  },
  Merged: {
    label: "Merged",
    className: "bg-emerald-100 text-emerald-700",
  },
  Closed: {
    label: "Closed",
    className: "bg-gray-100 text-gray-600",
  },
};

interface FeatureStatusBadgeProps {
  status: FeatureStatus;
}

export function FeatureStatusBadge({ status }: FeatureStatusBadgeProps) {
  const config = statusConfig[status] ?? { label: status, className: "bg-gray-100 text-gray-600" };
  return (
    <span className={`inline-flex items-center px-2 py-0.5 rounded text-xs font-medium ${config.className}`}>
      {config.label}
    </span>
  );
}
