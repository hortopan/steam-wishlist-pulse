<script lang="ts">
  import type { SnapshotEntry } from './types';
  import { METRIC_CONFIG, METRIC_KEYS } from './constants';
  import { formatNumber } from './utils';

  let { history }: { history: SnapshotEntry[] } = $props();

  let activeMetrics = $state<Set<string>>(new Set(METRIC_KEYS));
  let hoveredPoint = $state<{
    x: number;
    y: number;
    entry: SnapshotEntry;
    metric: string;
    value: number;
  } | null>(null);

  const CHART_W = 800;
  const CHART_H = 300;
  const PAD = { top: 20, right: 20, bottom: 40, left: 60 };

  function toggleMetric(metric: string) {
    const next = new Set(activeMetrics);
    if (next.has(metric)) {
      if (next.size > 1) next.delete(metric);
    } else {
      next.add(metric);
    }
    activeMetrics = next;
  }

  function buildChartData(entries: SnapshotEntry[], metrics: Set<string>) {
    if (entries.length === 0) return null;

    const activeKeys = METRIC_KEYS.filter((m) => metrics.has(m));

    let maxVal = 0;
    for (const e of entries) {
      for (const m of activeKeys) {
        const v = (e as any)[m] as number;
        if (v > maxVal) maxVal = v;
      }
    }
    if (maxVal === 0) maxVal = 1;

    const plotW = CHART_W - PAD.left - PAD.right;
    const plotH = CHART_H - PAD.top - PAD.bottom;

    const xScale = (i: number) =>
      PAD.left +
      (entries.length > 1 ? (i / (entries.length - 1)) * plotW : plotW / 2);
    const yScale = (v: number) => PAD.top + plotH - (v / maxVal) * plotH;

    const paths: Record<
      string,
      {
        d: string;
        points: {
          cx: number;
          cy: number;
          entry: SnapshotEntry;
          value: number;
        }[];
      }
    > = {};
    for (const m of activeKeys) {
      const pts = entries.map((e, i) => {
        const v = (e as any)[m] as number;
        return { cx: xScale(i), cy: yScale(v), entry: e, value: v };
      });
      const d = pts
        .map((p, i) => `${i === 0 ? "M" : "L"}${p.cx},${p.cy}`)
        .join(" ");
      paths[m] = { d, points: pts };
    }

    const yTicks = [];
    const step = maxVal / 4;
    for (let i = 0; i <= 4; i++) {
      const v = Math.round(step * i);
      yTicks.push({ y: yScale(v), label: formatNumber(v) });
    }

    const uniqueDates = new Set(entries.map((e) => e.date.split("T")[0]));
    const sameDay = uniqueDates.size === 1;
    const xLabels = entries.map((e, i) => ({
      x: xScale(i),
      label: sameDay
        ? (e.fetched_at || e.date).split("T")[1]?.slice(0, 5) ||
          e.date.split("T")[0].slice(5)
        : e.date.split("T")[0].slice(5),
    }));
    const maxLabels = 12;
    let filteredXLabels = xLabels;
    if (xLabels.length > maxLabels) {
      const every = Math.ceil(xLabels.length / maxLabels);
      filteredXLabels = xLabels.filter(
        (_, i) => i % every === 0 || i === xLabels.length - 1,
      );
    }

    return { paths, yTicks, xLabels: filteredXLabels, plotW, plotH };
  }

  let chartData = $derived(buildChartData(history, activeMetrics));

  $effect(() => {
    chartData;
    hoveredPoint = null;
  });
</script>

{#if chartData}
  <div class="chart-section">
    <div class="chart-header">
      <h2>Historical Trends</h2>
      <div class="chart-legend">
        {#each Object.entries(METRIC_CONFIG) as [key, cfg]}
          <button
            class="legend-btn"
            class:active={activeMetrics.has(key)}
            style="--metric-color: {cfg.color}"
            onclick={() => toggleMetric(key)}
          >
            <span class="legend-dot"></span>
            {cfg.label}
          </button>
        {/each}
      </div>
    </div>
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="chart-container" onmouseleave={() => (hoveredPoint = null)}>
      <svg
        viewBox="0 0 {CHART_W} {CHART_H}"
        preserveAspectRatio="xMidYMid meet"
      >
        {#each chartData.yTicks as tick}
          <line
            x1={PAD.left}
            y1={tick.y}
            x2={PAD.left + chartData.plotW}
            y2={tick.y}
            stroke="var(--border)"
            stroke-width="1"
            opacity="0.5"
          />
          <text
            x={PAD.left - 10}
            y={tick.y + 4}
            fill="var(--text-muted)"
            font-size="11"
            text-anchor="end">{tick.label}</text
          >
        {/each}

        {#each chartData.xLabels as lbl}
          <text
            x={lbl.x}
            y={CHART_H - 8}
            fill="var(--text-muted)"
            font-size="10"
            text-anchor="middle">{lbl.label}</text
          >
        {/each}

        {#each Object.entries(chartData.paths) as [metric, pathData]}
          <path
            d={pathData.d}
            fill="none"
            stroke={METRIC_CONFIG[metric].color}
            stroke-width="2.5"
            stroke-linecap="round"
            stroke-linejoin="round"
            opacity="0.9"
          />
          {#each pathData.points as pt}
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <circle
              cx={pt.cx}
              cy={pt.cy}
              r={hoveredPoint?.x === pt.cx &&
              hoveredPoint?.metric === metric
                ? 6
                : 4}
              fill={METRIC_CONFIG[metric].color}
              stroke="var(--surface)"
              stroke-width="2"
              style="cursor: pointer;"
              onmouseenter={() =>
                (hoveredPoint = {
                  x: pt.cx,
                  y: pt.cy,
                  entry: pt.entry,
                  metric,
                  value: pt.value,
                })}
            />
          {/each}
        {/each}
      </svg>

      {#if hoveredPoint}
        {@const tipX = (hoveredPoint.x / CHART_W) * 100}
        {@const tipY = (hoveredPoint.y / CHART_H) * 100}
        <div
          class="tooltip"
          style="left: {tipX}%; top: {tipY}%; transform: translate({tipX >
          80
            ? '-100%'
            : tipX < 20
              ? '0%'
              : '-50%'}, -120%);"
        >
          <div class="tooltip-date">
            {hoveredPoint.entry.date.split("T")[0]}
          </div>
          <div
            class="tooltip-value"
            style="color: {METRIC_CONFIG[hoveredPoint.metric].color}"
          >
            {METRIC_CONFIG[hoveredPoint.metric]
              .prefix}{hoveredPoint.value.toLocaleString()}
            {METRIC_CONFIG[hoveredPoint.metric].label}
          </div>
        </div>
      {/if}
    </div>
  </div>
{:else if history.length > 0}
  <div class="chart-section">
    <h2>Historical Trends</h2>
    <p class="chart-placeholder">Not enough data to display a chart yet.</p>
  </div>
{/if}

<style>
  .chart-section {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 0.75rem;
    padding: 1.5rem;
    margin-bottom: 1.5rem;
  }

  .chart-section h2 {
    font-size: 1.1rem;
    font-weight: 600;
    margin-bottom: 0;
  }

  .chart-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 1rem;
    flex-wrap: wrap;
    gap: 0.75rem;
  }

  .chart-legend {
    display: flex;
    gap: 0.5rem;
    flex-wrap: wrap;
  }

  .legend-btn {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    background: transparent;
    border: 1px solid var(--border);
    color: var(--text-muted);
    padding: 0.3rem 0.65rem;
    border-radius: 1rem;
    cursor: pointer;
    font-size: 0.75rem;
    transition: all 0.2s;
    opacity: 0.5;
  }

  .legend-btn.active {
    opacity: 1;
    border-color: var(--metric-color);
    color: var(--metric-color);
  }

  .legend-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--metric-color);
  }

  .chart-container {
    position: relative;
    width: 100%;
  }

  .chart-container svg {
    width: 100%;
    height: auto;
  }

  .chart-container svg path {
    transition: d 0.5s ease-out;
  }

  .chart-container svg circle {
    transition:
      cx 0.5s ease-out,
      cy 0.5s ease-out,
      r 0.15s ease-out;
  }

  .tooltip {
    position: absolute;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 0.5rem;
    padding: 0.5rem 0.75rem;
    pointer-events: none;
    white-space: nowrap;
    z-index: 10;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);
  }

  .tooltip-date {
    font-size: 0.7rem;
    color: var(--text-muted);
    margin-bottom: 0.2rem;
  }

  .tooltip-value {
    font-size: 0.85rem;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }

  .chart-placeholder {
    color: var(--text-muted);
    font-size: 0.9rem;
    padding: 2rem 0;
    text-align: center;
  }

  @media (max-width: 640px) {
    .chart-header {
      flex-direction: column;
      align-items: flex-start;
    }
  }
</style>
