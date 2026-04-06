import { useState, useEffect, useMemo } from "react";
import { api } from "../api/client";
import type { WorkflowTokenAggregate, WorkflowTokenTrendRow, StepTokenHeatmapRow, WorkflowRunMetricsRow, WorkflowFailureRateTrendRow, StepFailureHeatmapRow, WorkflowPercentiles, WorkflowRegressionSignal } from "../api/types";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";

type SortKey = "avg_input" | "avg_output" | "avg_cache_read" | "run_count";
type TrendGranularity = "daily" | "weekly";
type HistMetric = "duration" | "input_tokens" | "output_tokens";

function fmtK(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(Math.round(n));
}

function fmtDuration(ms: number | null): string {
  if (ms === null) return "—";
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(1)}s`;
  const m = Math.floor(s / 60);
  const rem = Math.round(s % 60);
  return `${m}m${rem.toString().padStart(2, "0")}s`;
}

function fmtCost(usd: number | null): string {
  if (usd === null) return "—";
  if (usd < 0.01) return `$${usd.toFixed(4)}`;
  return `$${usd.toFixed(2)}`;
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
  const [selectedBucketIdx, setSelectedBucketIdx] = useState<number | null>(null);

  const [failureTrend, setFailureTrend] = useState<WorkflowFailureRateTrendRow[]>([]);
  const [failureTrendLoading, setFailureTrendLoading] = useState(false);
  const [failureTrendError, setFailureTrendError] = useState<string | null>(null);

  const [failureHeatmap, setFailureHeatmap] = useState<StepFailureHeatmapRow[]>([]);
  const [failureHeatmapLoading, setFailureHeatmapLoading] = useState(false);
  const [failureHeatmapError, setFailureHeatmapError] = useState<string | null>(null);

  const [percentiles, setPercentiles] = useState<WorkflowPercentiles | null>(null);
  const [percentilesLoading, setPercentilesLoading] = useState(false);

  const [regressions, setRegressions] = useState<WorkflowRegressionSignal[]>([]);
  const [regressionsError, setRegressionsError] = useState<string | null>(null);
  const [regressionsOpen, setRegressionsOpen] = useState(true);

  useEffect(() => {
    setAggLoading(true);
    api.getWorkflowTokenAggregates()
      .then(setAggregates)
      .catch((e) => setAggError(e instanceof Error ? e.message : "Failed to load aggregates"))
      .finally(() => setAggLoading(false));
    api.getWorkflowRegressions()
      .then(setRegressions)
      .catch((e) => setRegressionsError(e instanceof Error ? e.message : "Failed to load regressions"));
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

  useEffect(() => {
    setSelectedBucketIdx(null);
  }, [selectedWorkflow, histMetric, histDays]);

  useEffect(() => {
    if (!selectedWorkflow) return;
    setFailureTrendLoading(true);
    setFailureTrendError(null);
    api.getWorkflowFailureRateTrend(selectedWorkflow, granularity)
      .then(setFailureTrend)
      .catch((e) => setFailureTrendError(e instanceof Error ? e.message : "Failed to load failure trend"))
      .finally(() => setFailureTrendLoading(false));
  }, [selectedWorkflow, granularity]);

  useEffect(() => {
    if (!selectedWorkflow) return;
    setFailureHeatmapLoading(true);
    setFailureHeatmapError(null);
    api.getStepFailureHeatmap(selectedWorkflow, 20)
      .then(setFailureHeatmap)
      .catch((e) => setFailureHeatmapError(e instanceof Error ? e.message : "Failed to load failure heatmap"))
      .finally(() => setFailureHeatmapLoading(false));
  }, [selectedWorkflow]);

  useEffect(() => {
    if (!selectedWorkflow) {
      setPercentiles(null);
      return;
    }
    setPercentilesLoading(true);
    api.getWorkflowPercentiles(selectedWorkflow, histDays)
      .then(setPercentiles)
      .catch(() => setPercentiles(null))
      .finally(() => setPercentilesLoading(false));
  }, [selectedWorkflow, histDays]);

  const sorted = [...aggregates].sort((a, b) => {
    const av = a[sortKey], bv = b[sortKey];
    return sortAsc ? av - bv : bv - av;
  });

  const regressionByWorkflow = useMemo(
    () => new Map(regressions.map((r) => [r.workflow_name, r])),
    [regressions],
  );

  const activeRegressions = useMemo(
    () => regressions.filter((r) => r.duration_regressed || r.cost_regressed || r.failure_rate_regressed),
    [regressions],
  );

  function buildRegressionTooltip(r: WorkflowRegressionSignal): string {
    const parts: string[] = [];
    if (r.duration_regressed && r.duration_change_pct !== null) {
      const from = fmtDuration(r.baseline_p75_duration_ms);
      const to = fmtDuration(r.recent_p75_duration_ms);
      parts.push(`P75 duration up ${r.duration_change_pct.toFixed(0)}% (${from} → ${to})`);
    }
    if (r.cost_regressed && r.cost_change_pct !== null) {
      const from = fmtCost(r.baseline_p75_cost_usd);
      const to = fmtCost(r.recent_p75_cost_usd);
      parts.push(`P75 cost up ${r.cost_change_pct.toFixed(0)}% (${from} → ${to})`);
    }
    if (r.failure_rate_regressed) {
      parts.push(`Failure rate up ${r.failure_rate_change_pp.toFixed(1)}pp (${r.baseline_failure_rate.toFixed(1)}% → ${r.recent_failure_rate.toFixed(1)}%)`);
    }
    return parts.join(" · ");
  }

  const histogramBins = useMemo(() => {
    const paired = runMetrics
      .map((r) => {
        const v = histMetric === "duration" ? r.duration_ms
          : histMetric === "input_tokens" ? r.input_tokens
          : r.output_tokens;
        return v !== null && v !== undefined && v > 0 ? { run: r, value: v } : null;
      })
      .filter((x): x is { run: WorkflowRunMetricsRow; value: number } => x !== null);

    if (paired.length < 5) return null;

    const values = paired.map((p) => p.value);
    const n = values.length;
    const k = Math.ceil(Math.log2(n)) + 1;
    const minVal = Math.min(...values);
    const maxVal = Math.max(...values);
    const range = maxVal - minVal;
    const width = range === 0 ? 1 : range / k;

    const bins: { label: string; runs: { runId: string; startedAt: string; worktreeId: string | null; repoId: string | null }[] }[] = Array.from({ length: k }, (_, i) => {
      const lo = minVal + i * width;
      const label = histMetric === "duration"
        ? `${(lo / 1000).toFixed(1)}s`
        : `${fmtK(lo)}`;
      return { label, runs: [] };
    });

    for (const { run, value } of paired) {
      const idx = Math.min(Math.floor((value - minVal) / width), k - 1);
      bins[idx].runs.push({ runId: run.run_id, startedAt: run.started_at, worktreeId: run.worktree_id ?? null, repoId: run.repo_id ?? null });
    }

    // Compute mean + stddev of bin counts for outlier highlighting
    const counts = bins.map((b) => b.runs.length);
    const mean = counts.reduce((a, c) => a + c, 0) / counts.length;
    const variance = counts.reduce((a, c) => a + (c - mean) ** 2, 0) / counts.length;
    const sigma = Math.sqrt(variance);
    const threshold = mean + 2 * sigma;

    const maxCount = Math.max(...counts, 1);
    return bins.map((b, i) => ({
      ...b,
      pct: Math.round((b.runs.length / maxCount) * 100),
      outlier: b.runs.length > threshold,
      isP95: false as boolean,
      binLo: minVal + i * width,
      binHi: minVal + (i + 1) * width,
    }));
  }, [runMetrics, histMetric]);

  const histogramBinsWithP95 = useMemo(() => {
    if (!histogramBins || histMetric !== "duration" || !percentiles?.p95_duration_ms) {
      return histogramBins;
    }
    const p95 = percentiles.p95_duration_ms;
    return histogramBins.map((b) => ({
      ...b,
      isP95: p95 >= b.binLo && p95 < b.binHi,
    }));
  }, [histogramBins, histMetric, percentiles]);

  function handleSort(key: SortKey) {
    if (sortKey === key) setSortAsc((p) => !p);
    else { setSortKey(key); setSortAsc(false); }
  }

  const SortIcon = ({ k }: { k: SortKey }) =>
    sortKey === k ? <span className="ml-1">{sortAsc ? "↑" : "↓"}</span> : null;

  function successRateBadge(rate: number) {
    const pct = Math.round(rate);
    const cls = rate >= 90
      ? "bg-green-100 text-green-700"
      : rate >= 70
      ? "bg-amber-100 text-amber-700"
      : "bg-red-100 text-red-700";
    return <span className={`inline-block px-1.5 py-0.5 rounded text-xs font-mono ${cls}`}>{pct}%</span>;
  }

  const maxHeatTok = heatmap.length > 0
    ? Math.max(...heatmap.map((r) => r.avg_input + r.avg_output))
    : 1;

  return (
    <div className="space-y-8">
      <div>
        <h2 className="text-xl font-bold text-gray-900">Workflow Token Analytics</h2>
        <p className="text-sm text-gray-500 mt-1">Token usage aggregated across completed workflow runs.</p>
      </div>

      {/* Regressions error */}
      {regressionsError && (
        <p className="text-sm text-red-500">{regressionsError}</p>
      )}

      {/* Regressions summary panel — shown only when at least one regression is detected */}
      {activeRegressions.length > 0 && (
        <section className="rounded-lg border border-amber-200 bg-amber-50 p-4">
          <button
            className="flex items-center gap-2 w-full text-left"
            onClick={() => setRegressionsOpen((o) => !o)}
          >
            <span className="text-amber-700 font-semibold text-sm">
              ⚠ {activeRegressions.length} Regression{activeRegressions.length !== 1 ? "s" : ""} Detected
            </span>
            <span className="ml-auto text-amber-500 text-xs">{regressionsOpen ? "▲ Hide" : "▼ Show"}</span>
          </button>
          {regressionsOpen && (
            <ul className="mt-3 space-y-2">
              {activeRegressions.map((r) => (
                <li key={r.workflow_name} className="text-xs text-amber-800">
                  <button
                    className="font-medium hover:underline text-amber-900"
                    onClick={() => setSelectedWorkflow(r.workflow_name)}
                  >
                    {r.workflow_title ?? r.workflow_name}
                  </button>
                  <span className="ml-2 text-amber-700">{buildRegressionTooltip(r)}</span>
                  <span className="ml-2 text-amber-500">({r.recent_runs} recent runs)</span>
                </li>
              ))}
            </ul>
          )}
        </section>
      )}

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
                  <th className="px-4 py-2 font-medium">Success Rate</th>
                  <th className="px-4 py-2 font-medium">Details</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-100">
                {sorted.map((row) => (
                  <tr
                    key={row.workflow_name}
                    className={`hover:bg-gray-50 ${selectedWorkflow === row.workflow_name ? "bg-indigo-50" : ""}`}
                  >
                    <td className="px-4 py-2 font-medium text-gray-800">
                      {row.workflow_title ?? row.workflow_name}
                      {(() => {
                        const sig = regressionByWorkflow.get(row.workflow_name);
                        if (!sig) return null;
                        const hasRegression = sig.duration_regressed || sig.cost_regressed || sig.failure_rate_regressed;
                        if (!hasRegression) return null;
                        return (
                          <span
                            className="ml-2 inline-block px-1.5 py-0.5 rounded text-xs bg-amber-100 text-amber-700 font-medium"
                            title={buildRegressionTooltip(sig)}
                          >
                            ⚠ Regression
                          </span>
                        );
                      })()}
                    </td>
                    <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{fmtK(row.avg_input)}</td>
                    <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{fmtK(row.avg_output)}</td>
                    <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{fmtK(row.avg_cache_read)}</td>
                    <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{row.run_count}</td>
                    <td className="px-4 py-2">{successRateBadge(row.success_rate)}</td>
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
            ) : histogramBinsWithP95 === null ? (
              <p className="text-sm text-gray-500">Not enough data (need at least 5 completed runs with {histMetric === "duration" ? "duration" : "token"} data).</p>
            ) : (
              <>
                <div className="rounded-lg border border-gray-200 bg-white overflow-hidden">
                  <div className="px-4 py-3 divide-y divide-gray-50">
                    {histogramBinsWithP95.map((bin, i) => (
                      <div
                        key={i}
                        className={`py-1 cursor-pointer rounded px-1 -mx-1 ${selectedBucketIdx === i ? "bg-indigo-50 ring-1 ring-indigo-200" : bin.isP95 ? "ring-1 ring-orange-300 bg-orange-50" : "hover:bg-gray-50"}`}
                        onClick={() => setSelectedBucketIdx(selectedBucketIdx === i ? null : i)}
                      >
                        <div className="flex items-center justify-between gap-2 mb-0.5">
                          <span className="text-xs font-mono text-gray-500 w-20 shrink-0">{bin.label}</span>
                          {bin.isP95 && <span className="text-xs font-semibold text-orange-500 shrink-0">P95</span>}
                          <span className="text-xs font-mono tabular-nums text-gray-400 shrink-0">{bin.runs.length}</span>
                        </div>
                        <div className="h-2 bg-gray-100 rounded-full overflow-hidden">
                          <div
                            className={`h-full rounded-full ${bin.isP95 ? "bg-orange-400" : bin.outlier ? "bg-amber-400" : "bg-indigo-400"}`}
                            style={{ width: `${bin.pct}%` }}
                          />
                        </div>
                      </div>
                    ))}
                  </div>
                </div>
                {selectedBucketIdx !== null && histogramBinsWithP95[selectedBucketIdx] && (
                  <div className="mt-3 rounded-lg border border-gray-200 bg-white p-4">
                    <div className="flex items-center justify-between mb-2">
                      <span className="text-xs font-semibold text-gray-600">
                        Runs in bucket "{histogramBinsWithP95[selectedBucketIdx].label}" — {histogramBinsWithP95[selectedBucketIdx].runs.length} runs
                      </span>
                      <button
                        onClick={() => setSelectedBucketIdx(null)}
                        className="text-xs text-gray-400 hover:text-gray-600"
                      >
                        ✕
                      </button>
                    </div>
                    <ul className="space-y-1">
                      {histogramBinsWithP95[selectedBucketIdx].runs.map(({ runId, startedAt, repoId, worktreeId }) => (
                        <li key={runId} className="text-xs font-mono flex items-center gap-2">
                          {repoId && worktreeId ? (
                            <a
                              href={`/repos/${repoId}/worktrees/${worktreeId}/workflows/runs/${runId}`}
                              className="text-indigo-500 hover:text-indigo-700 hover:underline"
                            >
                              {runId.slice(0, 12)}…
                            </a>
                          ) : (
                            <button
                              onClick={() => navigator.clipboard.writeText(runId)}
                              className="text-gray-500 hover:text-gray-700 cursor-copy"
                              title="Copy run ID"
                            >
                              {runId.slice(0, 12)}…
                            </button>
                          )}
                          <span className="text-gray-400">
                            {new Date(startedAt).toLocaleString()}
                          </span>
                        </li>
                      ))}
                    </ul>
                  </div>
                )}
              </>
            )}
          </section>

          {/* Section 4: Percentile summary */}
          <section>
            <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-3">
              Percentile Summary — {selectedWorkflow}
            </h3>
            {percentilesLoading ? (
              <LoadingSpinner />
            ) : percentiles === null ? (
              <p className="text-sm text-gray-500">No percentile data available (need at least one completed run with duration data).</p>
            ) : (
              <>
                {percentiles.run_count < 10 && (
                  <p className="text-xs text-amber-600 mb-2">Based on {percentiles.run_count} run{percentiles.run_count !== 1 ? "s" : ""} — estimates may be noisy with fewer than 10 runs.</p>
                )}
                <div className="rounded-lg border border-gray-200 bg-white overflow-hidden">
                  <table className="w-full text-xs">
                    <thead>
                      <tr className="border-b border-gray-200 bg-gray-50 text-gray-500 text-left">
                        <th className="px-4 py-2 font-medium">Metric</th>
                        <th className="px-4 py-2 font-medium tabular-nums">P50</th>
                        <th className="px-4 py-2 font-medium tabular-nums">P75</th>
                        <th className="px-4 py-2 font-medium tabular-nums text-orange-600">P95</th>
                        <th className="px-4 py-2 font-medium tabular-nums">P99</th>
                      </tr>
                    </thead>
                    <tbody className="divide-y divide-gray-100">
                      <tr className="hover:bg-gray-50">
                        <td className="px-4 py-2 text-gray-700 font-medium">Duration</td>
                        <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{fmtDuration(percentiles.p50_duration_ms)}</td>
                        <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{fmtDuration(percentiles.p75_duration_ms)}</td>
                        <td className="px-4 py-2 font-mono tabular-nums text-orange-600 font-semibold">{fmtDuration(percentiles.p95_duration_ms)}</td>
                        <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{fmtDuration(percentiles.p99_duration_ms)}</td>
                      </tr>
                      <tr className="hover:bg-gray-50">
                        <td className="px-4 py-2 text-gray-700 font-medium">Cost</td>
                        <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{fmtCost(percentiles.p50_cost_usd)}</td>
                        <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{fmtCost(percentiles.p75_cost_usd)}</td>
                        <td className="px-4 py-2 font-mono tabular-nums text-orange-600 font-semibold">{fmtCost(percentiles.p95_cost_usd)}</td>
                        <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{fmtCost(percentiles.p99_cost_usd)}</td>
                      </tr>
                      <tr className="hover:bg-gray-50">
                        <td className="px-4 py-2 text-gray-700 font-medium">Tokens</td>
                        <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{percentiles.p50_total_tokens !== null ? fmtK(percentiles.p50_total_tokens) : "—"}</td>
                        <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{percentiles.p75_total_tokens !== null ? fmtK(percentiles.p75_total_tokens) : "—"}</td>
                        <td className="px-4 py-2 font-mono tabular-nums text-orange-600 font-semibold">{percentiles.p95_total_tokens !== null ? fmtK(percentiles.p95_total_tokens) : "—"}</td>
                        <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{percentiles.p99_total_tokens !== null ? fmtK(percentiles.p99_total_tokens) : "—"}</td>
                      </tr>
                    </tbody>
                  </table>
                </div>
              </>
            )}
          </section>

          {/* Section 5: Failure rate over time (previously 4) */}
          <section>
            <div className="flex items-center gap-4 mb-3">
              <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400">
                Failure Rate Over Time — {selectedWorkflow}
              </h3>
            </div>
            {failureTrendLoading ? (
              <LoadingSpinner />
            ) : failureTrendError ? (
              <p className="text-sm text-red-500">{failureTrendError}</p>
            ) : failureTrend.length === 0 ? (
              <p className="text-sm text-gray-500">No failure trend data available.</p>
            ) : (
              <div className="rounded-lg border border-gray-200 bg-white overflow-hidden">
                <table className="w-full text-xs">
                  <thead>
                    <tr className="border-b border-gray-200 bg-gray-50 text-gray-500 text-left">
                      <th className="px-4 py-2 font-medium">Period</th>
                      <th className="px-4 py-2 font-medium tabular-nums">Total Runs</th>
                      <th className="px-4 py-2 font-medium tabular-nums">Failed</th>
                      <th className="px-4 py-2 font-medium">Success Rate</th>
                    </tr>
                  </thead>
                  <tbody className="divide-y divide-gray-100">
                    {failureTrend.map((row) => (
                      <tr key={row.period} className="hover:bg-gray-50">
                        <td className="px-4 py-2 font-mono text-gray-700">{row.period}</td>
                        <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{row.total_runs}</td>
                        <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{row.failed_runs}</td>
                        <td className="px-4 py-2">{successRateBadge(row.success_rate)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </section>

          {/* Section 5: Step failure heatmap */}
          <section>
            <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-3">
              Step Failure Heatmap — {selectedWorkflow}
            </h3>
            {failureHeatmapLoading ? (
              <LoadingSpinner />
            ) : failureHeatmapError ? (
              <p className="text-sm text-red-500">{failureHeatmapError}</p>
            ) : failureHeatmap.length === 0 ? (
              <p className="text-sm text-gray-500">No step failure data available.</p>
            ) : (
              <div className="rounded-lg border border-gray-200 bg-white overflow-hidden">
                <table className="w-full text-xs">
                  <thead>
                    <tr className="border-b border-gray-200 bg-gray-50 text-gray-500 text-left">
                      <th className="px-4 py-2 font-medium">Step</th>
                      <th className="px-4 py-2 font-medium tabular-nums">Executions</th>
                      <th className="px-4 py-2 font-medium tabular-nums">Failed</th>
                      <th className="px-4 py-2 font-medium tabular-nums">Failure Rate</th>
                      <th className="px-4 py-2 font-medium tabular-nums">Avg Retries</th>
                    </tr>
                  </thead>
                  <tbody className="divide-y divide-gray-100">
                    {failureHeatmap.map((row) => {
                      const rateCls = row.failure_rate >= 25
                        ? "bg-red-100 text-red-700"
                        : row.failure_rate >= 10
                        ? "bg-amber-100 text-amber-700"
                        : "text-gray-700";
                      return (
                        <tr key={row.step_name} className="hover:bg-gray-50">
                          <td className="px-4 py-2 text-gray-800">{row.step_name.replace(/^workflow:/, "")}</td>
                          <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{row.total_executions}</td>
                          <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{row.failed_executions}</td>
                          <td className="px-4 py-2">
                            <span className={`inline-block px-1.5 py-0.5 rounded font-mono ${rateCls}`}>
                              {row.failure_rate.toFixed(1)}%
                            </span>
                          </td>
                          <td className="px-4 py-2 font-mono tabular-nums text-gray-700">{row.avg_retry_count.toFixed(1)}</td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            )}
          </section>

          {/* Section 6: Step token heatmap */}
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
