import { useState, useEffect, useMemo } from "react";
import { api } from "../api/client";
import type { WorkflowTokenAggregate, WorkflowTokenTrendRow, StepTokenHeatmapRow, WorkflowRunMetricsRow } from "../api/types";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";

type SortKey = "avg_input" | "avg_output" | "avg_cache_read" | "run_count";
type TrendGranularity = "daily" | "weekly";
type HistMetric = "duration" | "input_tokens" | "output_tokens";

function fmtK(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(Math.round(n));
}

export function WorkflowAnalyticsPage() {
  const [aggregates, setAggregates] = useState<WorkflowTokenAggregate[]>([]);
  const [aggLoading, setAggLoading] = useState(true);
  const [aggError, setAggError] = useState<string | null>(null);
  const [sortKey, setSortKey] = useState<SortKey>("avg_input");
  const [sortAsc, setSortAsc] = useState(false);

  const [selectedWorkflow, setSelectedWorkflow] = useState<string | null>(null);
  const [granularity, setGranularity] = useState<TrendGranularity>("daily");
  const [trend, setTrend] = useState<WorkflowTokenTrendRow[]>([]);
  const [trendLoading, setTrendLoading] = useState(false);
  const [trendError, setTrendError] = useState<string | null>(null);

  const [heatmap, setHeatmap] = useState<StepTokenHeatmapRow[]>([]);
  const [heatmapLoading, setHeatmapLoading] = useState(false);
  const [heatmapError, setHeatmapError] = useState<string | null>(null);

  const [histMetric, setHistMetric] = useState<HistMetric>("duration");
  const [histDays, setHistDays] = useState<7 | 30 | 90>(30);
  const [runMetrics, setRunMetrics] = useState<WorkflowRunMetricsRow[]>([]);
  const [runMetricsLoading, setRunMetricsLoading] = useState(false);
  const [runMetricsError, setRunMetricsError] = useState<string | null>(null);

  useEffect(() => {
    setAggLoading(true);
    api.getWorkflowTokenAggregates()
      .then(setAggregates)
      .catch((e) => setAggError(e instanceof Error ? e.message : "Failed to load aggregates"))
      .finally(() => setAggLoading(false));
  }, []);

  useEffect(() => {
    if (!selectedWorkflow) return;
    setTrendLoading(true);
    setTrendError(null);
    api.getWorkflowTokenTrend(selectedWorkflow, granularity)
      .then(setTrend)
      .catch((e) => setTrendError(e instanceof Error ? e.message : "Failed to load trend"))
      .finally(() => setTrendLoading(false));
  }, [selectedWorkflow, granularity]);

  useEffect(() => {
    if (!selectedWorkflow) return;
    setHeatmapLoading(true);
    setHeatmapError(null);
    api.getStepTokenHeatmap(selectedWorkflow, 20)
      .then(setHeatmap)
      .catch((e) => setHeatmapError(e instanceof Error ? e.message : "Failed to load heatmap"))
      .finally(() => setHeatmapLoading(false));
  }, [selectedWorkflow]);

  useEffect(() => {
    if (!selectedWorkflow) return;
    setRunMetricsLoading(true);
    setRunMetricsError(null);
    api.getRunMetrics(selectedWorkflow, histDays)
      .then(setRunMetrics)
      .catch((e) => setRunMetricsError(e instanceof Error ? e.message : "Failed to load run metrics"))
      .finally(() => setRunMetricsLoading(false));
  }, [selectedWorkflow, histDays]);

  const sorted = [...aggregates].sort((a, b) => {
    const av = a[sortKey], bv = b[sortKey];
    return sortAsc ? av - bv : bv - av;
  });

  const histogramBins = useMemo(() => {
    const values = runMetrics
      .map((r) => {
        if (histMetric === "duration") return r.duration_ms;
        if (histMetric === "input_tokens") return r.input_tokens;
        return r.output_tokens;
      })
      .filter((v): v is number => v !== null && v !== undefined);

    if (values.length < 5) return null;

    const n = values.length;
    const k = Math.ceil(Math.log2(n)) + 1;
    const minVal = Math.min(...values);
    const maxVal = Math.max(...values);
    const range = maxVal - minVal;
    const width = range === 0 ? 1 : range / k;

    const bins: { label: string; count: number }[] = Array.from({ length: k }, (_, i) => {
      const lo = minVal + i * width;
      const hi = lo + width;
      const label = histMetric === "duration"
        ? `${(lo / 1000).toFixed(1)}s`
        : `${fmtK(lo)}`;
      return { label, count: 0 };
    });

    for (const v of values) {
      const idx = Math.min(Math.floor((v - minVal) / width), k - 1);
      bins[idx].count++;
    }

    // Compute mean + stddev of bin counts for outlier highlighting
    const counts = bins.map((b) => b.count);
    const mean = counts.reduce((a, c) => a + c, 0) / counts.length;
    const variance = counts.reduce((a, c) => a + (c - mean) ** 2, 0) / counts.length;
    const sigma = Math.sqrt(variance);
    const threshold = mean + 2 * sigma;

    const maxCount = Math.max(...counts, 1);
    return bins.map((b) => ({
      ...b,
      pct: Math.round((b.count / maxCount) * 100),
      outlier: b.count > threshold,
    }));
  }, [runMetrics, histMetric]);

  function handleSort(key: SortKey) {
    if (sortKey === key) setSortAsc((p) => !p);
    else { setSortKey(key); setSortAsc(false); }
  }

  const SortIcon = ({ k }: { k: SortKey }) =>
    sortKey === k ? <span className="ml-1">{sortAsc ? "↑" : "↓"}</span> : null;

  const maxHeatTok = heatmap.length > 0
    ? Math.max(...heatmap.map((r) => r.avg_input + r.avg_output))
    : 1;

  return (
    <div className="space-y-8">
      <div>
        <h2 className="text-xl font-bold text-gray-900">Workflow Token Analytics</h2>
        <p className="text-sm text-gray-500 mt-1">Token usage aggregated across completed workflow runs.</p>
      </div>

      {/* Section 1: Aggregate table */}
      <section>
        <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-3">
          Per-Workflow Averages
        </h3>
        {aggLoading ? (
          <LoadingSpinner />
        ) : aggError ? (
          <p className="text-sm text-red-500">{aggError}</p>
        ) : aggregates.length === 0 ? (
          <p className="text-sm text-gray-500">No completed runs with token data yet.</p>
        ) : (
          <div className="rounded-lg border border-gray-200 bg-white overflow-hidden">
            <table className="w-full text-xs">
              <thead>
                <tr className="border-b border-gray-200 bg-gray-50 text-gray-500 text-left">
                  <th className="px-4 py-2 font-medium">Workflow</th>
                  <th
                    className="px-4 py-2 font-medium cursor-pointer hover:text-gray-700 tabular-nums"
                    onClick={() => handleSort("avg_input")}
                  >
                    Avg Input<SortIcon k="avg_input" />
                  </th>
                  <th
                    className="px-4 py-2 font-medium cursor-pointer hover:text-gray-700 tabular-nums"
                    onClick={() => handleSort("avg_output")}
                  >
                    Avg Output<SortIcon k="avg_output" />
                  </th>
                  <th
                    className="px-4 py-2 font-medium cursor-pointer hover:text-gray-700 tabular-nums"
                    onClick={() => handleSort("avg_cache_read")}
                  >
                    Avg Cache Read<SortIcon k="avg_cache_read" />
                  </th>
                  <th
                    className="px-4 py-2 font-medium cursor-pointer hover:text-gray-700 tabular-nums"
                    onClick={() => handleSort("run_count")}
                  >
                    Runs<SortIcon k="run_count" />
                  </th>
                  <th className="px-4 py-2 font-medium">Details</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-100">
                {sorted.map((row) => (
                  <tr
                    key={row.workflow_name}
                    className={`hover:bg-gray-50 ${selectedWorkflow === row.workflow_name ? "bg-indigo-50" : ""}`}
                  >
                    <td className="px-4 py-2 font-medium text-gray-800">{row.workflow_name}</td>
                    <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{fmtK(row.avg_input)}</td>
                    <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{fmtK(row.avg_output)}</td>
                    <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{fmtK(row.avg_cache_read)}</td>
                    <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{row.run_count}</td>
                    <td className="px-4 py-2">
                      <button
                        onClick={() => setSelectedWorkflow(
                          selectedWorkflow === row.workflow_name ? null : row.workflow_name
                        )}
                        className="text-indigo-600 hover:underline text-xs"
                      >
                        {selectedWorkflow === row.workflow_name ? "Hide" : "Drill in"}
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      {/* Sections 2 & 3: only shown when a workflow is selected */}
      {selectedWorkflow && (
        <>
          {/* Section 2: Token trend over time */}
          <section>
            <div className="flex items-center gap-4 mb-3">
              <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400">
                Token Trend — {selectedWorkflow}
              </h3>
              <div className="flex items-center gap-2 text-xs">
                <button
                  onClick={() => setGranularity("daily")}
                  className={`px-2 py-0.5 rounded ${granularity === "daily" ? "bg-indigo-100 text-indigo-700 font-medium" : "text-gray-500 hover:text-gray-700"}`}
                >
                  Daily
                </button>
                <button
                  onClick={() => setGranularity("weekly")}
                  className={`px-2 py-0.5 rounded ${granularity === "weekly" ? "bg-indigo-100 text-indigo-700 font-medium" : "text-gray-500 hover:text-gray-700"}`}
                >
                  Weekly
                </button>
              </div>
            </div>
            {trendLoading ? (
              <LoadingSpinner />
            ) : trendError ? (
              <p className="text-sm text-red-500">{trendError}</p>
            ) : trend.length === 0 ? (
              <p className="text-sm text-gray-500">No trend data available.</p>
            ) : (
              <div className="rounded-lg border border-gray-200 bg-white overflow-hidden">
                <table className="w-full text-xs">
                  <thead>
                    <tr className="border-b border-gray-200 bg-gray-50 text-gray-500 text-left">
                      <th className="px-4 py-2 font-medium">Period</th>
                      <th className="px-4 py-2 font-medium tabular-nums">Total Input</th>
                      <th className="px-4 py-2 font-medium tabular-nums">Total Output</th>
                      <th className="px-4 py-2 font-medium tabular-nums">Total Cache Read</th>
                    </tr>
                  </thead>
                  <tbody className="divide-y divide-gray-100">
                    {trend.map((row) => (
                      <tr key={row.period} className="hover:bg-gray-50">
                        <td className="px-4 py-2 font-mono text-gray-700">{row.period}</td>
                        <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{row.total_input.toLocaleString()}</td>
                        <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{row.total_output.toLocaleString()}</td>
                        <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{row.total_cache_read.toLocaleString()}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </section>

          {/* Section 3: Run distribution histogram */}
          <section>
            <div className="flex items-center gap-4 mb-3">
              <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400">
                Run Distribution — {selectedWorkflow}
              </h3>
              <div className="flex items-center gap-2 text-xs">
                <button
                  onClick={() => setHistMetric("duration")}
                  className={`px-2 py-0.5 rounded ${histMetric === "duration" ? "bg-indigo-100 text-indigo-700 font-medium" : "text-gray-500 hover:text-gray-700"}`}
                >
                  Duration
                </button>
                <button
                  onClick={() => setHistMetric("input_tokens")}
                  className={`px-2 py-0.5 rounded ${histMetric === "input_tokens" ? "bg-indigo-100 text-indigo-700 font-medium" : "text-gray-500 hover:text-gray-700"}`}
                >
                  Input Tokens
                </button>
                <button
                  onClick={() => setHistMetric("output_tokens")}
                  className={`px-2 py-0.5 rounded ${histMetric === "output_tokens" ? "bg-indigo-100 text-indigo-700 font-medium" : "text-gray-500 hover:text-gray-700"}`}
                >
                  Output Tokens
                </button>
              </div>
              <div className="flex items-center gap-2 text-xs ml-auto">
                {([7, 30, 90] as const).map((d) => (
                  <button
                    key={d}
                    onClick={() => setHistDays(d)}
                    className={`px-2 py-0.5 rounded ${histDays === d ? "bg-indigo-100 text-indigo-700 font-medium" : "text-gray-500 hover:text-gray-700"}`}
                  >
                    {d}d
                  </button>
                ))}
              </div>
            </div>
            {runMetricsLoading ? (
              <LoadingSpinner />
            ) : runMetricsError ? (
              <p className="text-sm text-red-500">{runMetricsError}</p>
            ) : histogramBins === null ? (
              <p className="text-sm text-gray-500">Not enough data (need at least 5 completed runs with {histMetric === "duration" ? "duration" : "token"} data).</p>
            ) : (
              <div className="rounded-lg border border-gray-200 bg-white overflow-hidden">
                <div className="px-4 py-3 divide-y divide-gray-50">
                  {histogramBins.map((bin, i) => (
                    <div key={i} className="py-1">
                      <div className="flex items-center justify-between gap-2 mb-0.5">
                        <span className="text-xs font-mono text-gray-500 w-20 shrink-0">{bin.label}</span>
                        <span className="text-xs font-mono tabular-nums text-gray-400 shrink-0">{bin.count}</span>
                      </div>
                      <div className="h-2 bg-gray-100 rounded-full overflow-hidden">
                        <div
                          className={`h-full rounded-full ${bin.outlier ? "bg-amber-400" : "bg-indigo-400"}`}
                          style={{ width: `${bin.pct}%` }}
                        />
                      </div>
                    </div>
                  ))}
                </div>
              </div>
            )}
          </section>

          {/* Section 4: Step token heatmap */}
          <section>
            <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-3">
              Step Token Heatmap — {selectedWorkflow}
            </h3>
            {heatmapLoading ? (
              <LoadingSpinner />
            ) : heatmapError ? (
              <p className="text-sm text-red-500">{heatmapError}</p>
            ) : heatmap.length === 0 ? (
              <p className="text-sm text-gray-500">No per-step token data available.</p>
            ) : (
              <div className="rounded-lg border border-gray-200 bg-white overflow-hidden">
                <div className="divide-y divide-gray-100">
                  {heatmap.map((row) => {
                    const total = row.avg_input + row.avg_output;
                    const pct = maxHeatTok > 0 ? Math.round((total / maxHeatTok) * 100) : 0;
                    return (
                      <div key={row.step_name} className="px-4 py-2">
                        <div className="flex items-center justify-between gap-2 mb-1">
                          <span className="text-xs text-gray-700">{row.step_name.replace(/^workflow:/, "")}</span>
                          <div className="flex items-center gap-3 text-xs font-mono tabular-nums text-gray-500 shrink-0">
                            <span>↑{fmtK(row.avg_input)}</span>
                            <span>↓{fmtK(row.avg_output)}</span>
                            <span className="text-gray-400">{row.run_count} runs</span>
                          </div>
                        </div>
                        <div className="h-1.5 bg-gray-100 rounded-full overflow-hidden">
                          <div
                            className="h-full bg-indigo-400 rounded-full"
                            style={{ width: `${pct}%` }}
                          />
                        </div>
                      </div>
                    );
                  })}
                </div>
              </div>
            )}
          </section>
        </>
      )}
    </div>
  );
}
