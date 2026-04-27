<script lang="ts">
  import type { ChartPoint } from './types';
  import { METRIC_CONFIG, METRIC_KEYS } from './constants';
  import { formatNumber } from './utils';

  let {
    history,
    resolution,
    chartRange,
    onRangeChange,
    ranges,
    loading = false,
    customFrom = "",
    customTo = "",
    onCustomFromChange,
    onCustomToChange,
    onApplyCustom,
  }: {
    history: ChartPoint[];
    resolution: string;
    chartRange: string;
    onRangeChange: (range: any) => void;
    ranges: { key: string; label: string }[];
    loading?: boolean;
    customFrom?: string;
    customTo?: string;
    onCustomFromChange?: (v: string) => void;
    onCustomToChange?: (v: string) => void;
    onApplyCustom?: () => void;
  } = $props();

  let activeMetrics = $state<Set<string>>(new Set(METRIC_KEYS));
  let hoveredPoint = $state<{
    x: number;
    y: number;
    entry: ChartPoint;
    metric: string;
    value: number;
    prevValue: number | null;
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

  function buildChartData(entries: ChartPoint[], metrics: Set<string>) {
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
          entry: ChartPoint;
          value: number;
          prevValue: number | null;
        }[];
      }
    > = {};
    for (const m of activeKeys) {
      const pts = entries.map((e, i) => {
        const v = (e as any)[m] as number;
        const pv = i > 0 ? ((entries[i - 1] as any)[m] as number) : null;
        return { cx: xScale(i), cy: yScale(v), entry: e, value: v, prevValue: pv };
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

    // Format X labels based on resolution
    const xLabels = entries.map((e, i) => {
      let label: string;
      if (resolution === "raw") {
        // For raw, show time if same day, otherwise date
        const dateStr = e.label.split("T")[0]?.slice(5) ?? e.label;
        const timeStr = e.label.split("T")[1]?.slice(0, 5);
        const uniqueDates = new Set(entries.map(en => en.label.split("T")[0]));
        label = uniqueDates.size === 1 && timeStr ? timeStr : dateStr;
      } else if (resolution === "weekly") {
        label = e.label; // "2025-W03" format
      } else if (resolution === "monthly") {
        label = e.label; // "2025-01" format
      } else {
        // daily
        label = e.label.slice(5); // "MM-DD"
      }
      return { x: xScale(i), label };
    });

    const maxLabels = 12;
    let filteredXLabels = xLabels;
    if (xLabels.length > maxLabels) {
      const every = Math.ceil(xLabels.length / maxLabels);
      filteredXLabels = xLabels.filter(
        (_, i) => i % every === 0 || i === xLabels.length - 1,
      );
    }

    // Collect anomaly marker positions (one per anomalous metric per chart point)
    const anomalyPoints: { x: number; y: number; entry: ChartPoint; metric: string; direction: 'up' | 'down' }[] = [];
    for (let i = 0; i < entries.length; i++) {
      const am = entries[i].anomaly_metrics;
      if (!am) continue;
      const x = xScale(i);
      for (const m of activeKeys) {
        if (!(am as any)[m]) continue;
        const v = (entries[i] as any)[m] as number;
        const y = yScale(v);
        // Determine direction from description or previous value
        const desc = (am.descriptions ?? []).find(d => d.toLowerCase().startsWith(m));
        const isUp = desc ? /above|spike/i.test(desc) : (i > 0 ? v > ((entries[i - 1] as any)[m] as number) : true);
        anomalyPoints.push({ x, y, entry: entries[i], metric: m, direction: isUp ? 'up' : 'down' });
      }
    }

    // Build lookup: "pointIndex-metric" → direction
    const anomalyDir = new Map<string, 'up' | 'down'>();
    for (const ap of anomalyPoints) {
      anomalyDir.set(`${ap.x}-${ap.metric}`, ap.direction);
    }

    return { paths, yTicks, xLabels: filteredXLabels, plotW, plotH, anomalyPoints, anomalyDir };
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
      <div class="chart-controls">
        <div class="range-selector">
          {#each ranges as r}
            <button
              class="range-btn"
              class:active={chartRange === r.key}
              onclick={() => onRangeChange(r.key)}
            >
              {r.label}
            </button>
          {/each}
        </div>
        {#if chartRange === "custom" && onApplyCustom && onCustomFromChange && onCustomToChange}
          <div class="custom-range">
            <label>
              From
              <input
                type="date"
                value={customFrom}
                max={customTo || undefined}
                oninput={(e) => onCustomFromChange!((e.currentTarget as HTMLInputElement).value)}
              />
            </label>
            <label>
              To
              <input
                type="date"
                value={customTo}
                min={customFrom || undefined}
                oninput={(e) => onCustomToChange!((e.currentTarget as HTMLInputElement).value)}
              />
            </label>
            <button
              class="range-btn apply"
              onclick={onApplyCustom}
              disabled={!customFrom || !customTo || customFrom > customTo}
            >
              Apply
            </button>
          </div>
        {/if}
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
    </div>
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="chart-container" class:chart-loading={loading} onmouseleave={() => (hoveredPoint = null)}>
      {#if loading}
        <div class="chart-overlay">
          <div class="chart-spinner"></div>
        </div>
      {/if}
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
            {@const isAnomaly = !!(pt.entry.anomaly_metrics as any)?.[metric]}
            {@const isHovered = hoveredPoint?.x === pt.cx && hoveredPoint?.metric === metric}
            {@const dir = isAnomaly ? chartData.anomalyDir.get(`${pt.cx}-${metric}`) : null}
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            {#if isAnomaly}
              <circle
                cx={pt.cx}
                cy={pt.cy}
                r={isHovered ? 14 : 11}
                fill="none"
                stroke={METRIC_CONFIG[metric].color}
                stroke-width="1.5"
                opacity="0.4"
                class="anomaly-ring"
              />
            {/if}
            <circle
              cx={pt.cx}
              cy={pt.cy}
              r={isHovered ? 8 : isAnomaly ? 7 : 4}
              fill={isAnomaly
                ? dir === 'up' ? METRIC_CONFIG[metric].color : 'var(--surface)'
                : METRIC_CONFIG[metric].color}
              stroke={METRIC_CONFIG[metric].color}
              stroke-width={isAnomaly ? 2.5 : 2}
              opacity={isAnomaly && dir === 'down' ? 0.6 : 1}
              style="cursor: pointer;"
              onmouseenter={() =>
                (hoveredPoint = {
                  x: pt.cx,
                  y: pt.cy,
                  entry: pt.entry,
                  metric,
                  value: pt.value,
                  prevValue: pt.prevValue,
                })}
            />
            {#if isAnomaly}
              <text
                x={pt.cx}
                y={pt.cy + (dir === 'down' ? 0.5 : -0.5)}
                fill={dir === 'up' ? 'var(--surface)' : METRIC_CONFIG[metric].color}
                font-size="9"
                font-weight="bold"
                text-anchor="middle"
                dominant-baseline="central"
                style="pointer-events: none;"
              >{dir === 'up' ? '▲' : '▼'}</text>
            {/if}
          {/each}
        {/each}
      </svg>

      {#if hoveredPoint}
        {@const tipX = (hoveredPoint.x / CHART_W) * 100}
        {@const tipY = (hoveredPoint.y / CHART_H) * 100}
        {@const hpAnomaly = !!(hoveredPoint.entry.anomaly_metrics as any)?.[hoveredPoint.metric]}
        {@const anomalyDescs = hpAnomaly ? (hoveredPoint!.entry.anomaly_metrics?.descriptions ?? []).filter(d => d.toLowerCase().startsWith(hoveredPoint!.metric)) : []}
        <div
          class="tooltip"
          class:anomaly-tooltip={hpAnomaly}
          style="left: {tipX}%; top: {tipY}%; transform: translate({tipX >
          80
            ? '-100%'
            : tipX < 20
              ? '0%'
              : '-50%'}, -120%);{hpAnomaly ? ` border-color: ${METRIC_CONFIG[hoveredPoint.metric].color};` : ''}"
        >
          <div class="tooltip-date">
            {hoveredPoint.entry.label}
          </div>
          <div
            class="tooltip-value"
            style="color: {METRIC_CONFIG[hoveredPoint.metric].color}"
          >
            {hoveredPoint.value.toLocaleString()}
            {METRIC_CONFIG[hoveredPoint.metric].label}
            {#if hoveredPoint.prevValue !== null}
              {@const delta = hoveredPoint.value - hoveredPoint.prevValue}
              <span class="tooltip-delta" class:positive={delta > 0} class:negative={delta < 0}>({delta > 0 ? '+' : ''}{delta.toLocaleString()})</span>
            {/if}
          </div>
          {#if anomalyDescs.length > 0}
            <div class="anomaly-divider"></div>
            {#each anomalyDescs as desc}
              <div class="anomaly-desc">{desc}</div>
            {/each}
          {/if}
          <div class="tooltip-resolution">{resolution}</div>
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

  .chart-controls {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    flex-wrap: wrap;
  }

  .range-selector {
    display: flex;
    gap: 0;
    border: 1px solid var(--border);
    border-radius: 0.5rem;
    overflow: hidden;
  }

  .range-btn {
    background: transparent;
    border: none;
    border-right: 1px solid var(--border);
    color: var(--text-muted);
    padding: 0.3rem 0.65rem;
    cursor: pointer;
    font-size: 0.75rem;
    font-weight: 500;
    transition: all 0.2s;
  }

  .range-btn:last-child {
    border-right: none;
  }

  .range-btn.active {
    background: var(--accent);
    color: white;
  }

  .range-btn:hover:not(.active) {
    background: rgba(99, 102, 241, 0.1);
    color: var(--accent);
  }

  .range-btn[disabled] {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .custom-range {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
    font-size: 0.75rem;
    color: var(--text-muted);
  }

  .custom-range label {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
  }

  .custom-range input[type="date"] {
    background: transparent;
    border: 1px solid var(--border);
    border-radius: 0.4rem;
    color: var(--text);
    padding: 0.25rem 0.5rem;
    font-size: 0.75rem;
    font-family: inherit;
  }

  .custom-range .apply {
    border: 1px solid var(--border);
    border-radius: 0.4rem;
    padding: 0.3rem 0.75rem;
  }

  .custom-range .apply:not([disabled]) {
    background: var(--accent);
    color: white;
    border-color: var(--accent);
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

  .chart-container.chart-loading svg {
    opacity: 0.35;
    transition: opacity 0.2s ease;
  }

  .chart-overlay {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 5;
    pointer-events: none;
  }

  .chart-spinner {
    width: 1.5rem;
    height: 1.5rem;
    border: 3px solid var(--border);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: chart-spin 0.8s linear infinite;
  }

  @keyframes chart-spin {
    to { transform: rotate(360deg); }
  }

  .chart-container svg {
    width: 100%;
    height: auto;
  }

  .chart-container svg circle {
    transition: r 0.15s ease-out;
  }

  .chart-container svg .anomaly-ring {
    animation: anomaly-pulse 2s ease-in-out infinite;
  }

  @keyframes anomaly-pulse {
    0%, 100% { opacity: 0.15; }
    50% { opacity: 0.5; }
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

  .tooltip-delta {
    font-size: 0.75rem;
    font-weight: 500;
    margin-left: 0.3rem;
    color: var(--text-muted);
  }

  .tooltip-delta.positive {
    color: var(--green);
  }

  .tooltip-delta.negative {
    color: var(--red);
  }

  .tooltip-resolution {
    font-size: 0.65rem;
    color: var(--text-muted);
    margin-top: 0.15rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .anomaly-tooltip {
    border-width: 1.5px;
  }


  .anomaly-divider {
    border-top: 1px solid var(--border);
    margin: 0.3rem 0;
  }

  .anomaly-desc {
    font-size: 0.75rem;
    color: var(--text-muted);
    font-weight: 500;
    font-style: italic;
  }

  .anomaly-desc + .anomaly-desc {
    margin-top: 0.15rem;
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
