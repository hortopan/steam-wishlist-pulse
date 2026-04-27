<script lang="ts">
  import type { CountryEntry } from './types';
  import { METRIC_CONFIG, METRIC_KEYS } from './constants';
  import { formatNumber } from './utils';

  let {
    countries,
    rangeLabel = '',
    loading = false,
  }: {
    countries: CountryEntry[];
    rangeLabel?: string;
    loading?: boolean;
  } = $props();

  let selectedMetric = $state<string>('adds');
  let hoveredIndex = $state<number | null>(null);
  let tooltipPos = $state<{ x: number; y: number } | null>(null);
  let showOtherBreakdown = $state(false);

  const TOP_N = 10;
  const SIZE = 300;
  const CX = SIZE / 2;
  const CY = SIZE / 2;
  const RADIUS = 120;
  const HOVER_OFFSET = 8;

  // Largest → smallest: warm red → orange → amber → yellow → green → teal → blue → indigo → purple → pink → gray (Other)
  const SLICE_COLORS = [
    '#ef4444', '#f97316', '#f59e0b', '#eab308', '#22c55e',
    '#14b8a6', '#06b6d4', '#3b82f6', '#6366f1', '#8b5cf6',
    '#94a3b8',
  ];

  function countryFlag(code: string): string {
    if (!code || !/^[a-zA-Z]{2}$/.test(code)) return '';
    return [...code.toUpperCase()].map(c => String.fromCodePoint(0x1F1E6 + c.charCodeAt(0) - 65)).join('');
  }

  let otherCountries = $derived.by(() => {
    if (!countries || countries.length === 0) return [];
    const metric = selectedMetric as keyof CountryEntry;
    const sorted = [...countries].sort((a, b) => (b[metric] as number) - (a[metric] as number));
    return sorted
      .slice(TOP_N)
      .filter(c => (c[metric] as number) > 0)
      .map(c => ({ code: c.country_code, value: c[metric] as number }));
  });

  let slices = $derived.by(() => {
    if (!countries || countries.length === 0) return [];

    const metric = selectedMetric as keyof CountryEntry;
    const sorted = [...countries].sort((a, b) => (b[metric] as number) - (a[metric] as number));

    const top = sorted.slice(0, TOP_N);
    const rest = sorted.slice(TOP_N);
    const otherValue = rest.reduce((sum, c) => sum + (c[metric] as number), 0);

    type SliceEntry = { label: string; code: string; value: number; color: string };
    const entries: SliceEntry[] = top
      .filter(c => (c[metric] as number) > 0)
      .map((c, i) => ({
        label: `${countryFlag(c.country_code)} ${c.country_code}`,
        code: c.country_code,
        value: c[metric] as number,
        color: SLICE_COLORS[i],
      }));

    if (otherValue > 0) {
      entries.push({
        label: 'Other',
        code: 'OTHER',
        value: otherValue,
        color: SLICE_COLORS[TOP_N],
      });
    }

    const total = entries.reduce((s, e) => s + e.value, 0);
    if (total === 0) return [];

    let currentAngle = -Math.PI / 2;
    return entries.map((entry, i) => {
      const pct = entry.value / total;
      const angle = pct * 2 * Math.PI;
      const startAngle = currentAngle;
      const endAngle = currentAngle + angle;
      currentAngle = endAngle;

      const midAngle = startAngle + angle / 2;

      const x1 = CX + RADIUS * Math.cos(startAngle);
      const y1 = CY + RADIUS * Math.sin(startAngle);
      const x2 = CX + RADIUS * Math.cos(endAngle);
      const y2 = CY + RADIUS * Math.sin(endAngle);
      const largeArc = angle > Math.PI ? 1 : 0;

      // For a single 100% slice, draw a full circle
      let d: string;
      if (pct >= 0.9999) {
        const r = RADIUS;
        d = `M ${CX},${CY - r} A ${r},${r} 0 1,1 ${CX},${CY + r} A ${r},${r} 0 1,1 ${CX},${CY - r} Z`;
      } else {
        d = `M ${CX},${CY} L ${x1},${y1} A ${RADIUS},${RADIUS} 0 ${largeArc},1 ${x2},${y2} Z`;
      }

      const hoverTx = HOVER_OFFSET * Math.cos(midAngle);
      const hoverTy = HOVER_OFFSET * Math.sin(midAngle);

      // Position flag label at ~65% of radius along bisector
      const labelR = RADIUS * 0.65;
      const labelX = CX + labelR * Math.cos(midAngle);
      const labelY = CY + labelR * Math.sin(midAngle);
      const showFlag = pct >= 0.05;

      return {
        ...entry,
        d,
        pct,
        midAngle,
        hoverTx,
        hoverTy,
        labelX,
        labelY,
        showFlag,
        total,
        index: i,
      };
    });
  });

  function handleSliceEnter(e: MouseEvent, index: number) {
    hoveredIndex = index;
    const rect = (e.currentTarget as SVGElement).closest('svg')!.getBoundingClientRect();
    tooltipPos = {
      x: e.clientX - rect.left,
      y: e.clientY - rect.top,
    };
  }

  function handleSliceMove(e: MouseEvent) {
    if (hoveredIndex === null) return;
    const rect = (e.currentTarget as SVGElement).closest('svg')!.getBoundingClientRect();
    tooltipPos = {
      x: e.clientX - rect.left,
      y: e.clientY - rect.top,
    };
  }

  function handleSliceLeave() {
    hoveredIndex = null;
    tooltipPos = null;
  }
</script>

<div class="pie-section">
  <div class="pie-header">
    <h2>Country Distribution {#if rangeLabel}<span class="range-badge">({rangeLabel})</span>{/if}</h2>
    <div class="metric-selector">
      {#each METRIC_KEYS as key}
        {@const cfg = METRIC_CONFIG[key]}
        <button
          class="metric-btn"
          class:active={selectedMetric === key}
          style="--metric-color: {cfg.color}"
          onclick={() => { selectedMetric = key; showOtherBreakdown = false; }}
        >
          <span class="metric-dot"></span>
          {cfg.label}
        </button>
      {/each}
    </div>
  </div>

  {#if slices.length === 0}
    <p class="pie-empty">No country data for this metric in the selected range.</p>
  {:else}
    <div class="pie-layout">
      <div class="pie-container" class:pie-loading={loading}>
        {#if loading}
          <div class="pie-overlay"><div class="pie-spinner"></div></div>
        {/if}
        <svg viewBox="0 0 {SIZE} {SIZE}" width="{SIZE}" height="{SIZE}">
          {#each slices as slice, i}
            <path
              role="img"
              aria-label="{slice.label}: {slice.value.toLocaleString()} ({(slice.pct * 100).toFixed(1)}%)"
              d={slice.d}
              fill={slice.color}
              stroke="var(--surface)"
              stroke-width="2"
              transform={hoveredIndex === i ? `translate(${slice.hoverTx}, ${slice.hoverTy})` : ''}
              style="transition: transform 0.15s ease; cursor: pointer; opacity: {hoveredIndex !== null && hoveredIndex !== i ? 0.6 : 1};"
              onmouseenter={(e) => handleSliceEnter(e, i)}
              onmousemove={handleSliceMove}
              onmouseleave={handleSliceLeave}
            />
          {/each}
          {#each slices as slice, i}
            {#if slice.showFlag}
              <text
                x={slice.labelX}
                y={slice.labelY}
                text-anchor="middle"
                dominant-baseline="central"
                font-size={slice.pct >= 0.12 ? '20' : '16'}
                transform={hoveredIndex === i ? `translate(${slice.hoverTx}, ${slice.hoverTy})` : ''}
                style="transition: transform 0.15s ease; pointer-events: none;"
              >{slice.code === 'OTHER' ? '🌍' : countryFlag(slice.code)}</text>
            {/if}
          {/each}
        </svg>
        {#if hoveredIndex !== null && tooltipPos && slices[hoveredIndex]}
          {@const s = slices[hoveredIndex]}
          <div
            class="pie-tooltip"
            style="left: {tooltipPos.x}px; top: {tooltipPos.y}px;"
          >
            <div class="pie-tooltip-label">{s.label}</div>
            <div class="pie-tooltip-value">{s.value.toLocaleString()} ({(s.pct * 100).toFixed(1)}%)</div>
          </div>
        {/if}
      </div>
      <div class="pie-legend">
        {#each slices as slice, i}
          {@const isOther = slice.code === 'OTHER'}
          {@const otherClickable = isOther && otherCountries.length > 0}
          <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
          <div
            role={otherClickable ? 'button' : 'listitem'}
            tabindex={otherClickable ? 0 : undefined}
            class="pie-legend-item"
            class:dimmed={hoveredIndex !== null && hoveredIndex !== i}
            class:clickable={otherClickable}
            onmouseenter={() => { hoveredIndex = i; }}
            onmouseleave={() => { hoveredIndex = null; }}
            onclick={() => { if (otherClickable) showOtherBreakdown = !showOtherBreakdown; }}
            onkeydown={(e) => { if (otherClickable && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); showOtherBreakdown = !showOtherBreakdown; } }}
          >
            <span class="pie-legend-dot" style="background: {slice.color}"></span>
            <span class="pie-legend-label">
              {slice.label}
              {#if isOther && otherCountries.length > 0}
                <span class="pie-legend-chevron">{showOtherBreakdown ? '▲' : '▼'}</span>
              {/if}
            </span>
            <span class="pie-legend-value">{formatNumber(slice.value)}</span>
            <span class="pie-legend-pct">{(slice.pct * 100).toFixed(1)}%</span>
          </div>
          {#if isOther && showOtherBreakdown && otherCountries.length > 0}
            <div class="pie-legend-sublist">
              {#each otherCountries as oc}
                <div class="pie-legend-subitem">
                  <span class="pie-legend-label">{countryFlag(oc.code)} {oc.code}</span>
                  <span class="pie-legend-value">{formatNumber(oc.value)}</span>
                  <span class="pie-legend-pct">{((oc.value / slice.total) * 100).toFixed(1)}%</span>
                </div>
              {/each}
            </div>
          {/if}
        {/each}
      </div>
    </div>
  {/if}
</div>

<style>
  .pie-section {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 0.75rem;
    padding: 1.5rem;
    margin-bottom: 1.5rem;
  }

  .pie-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 1rem;
    flex-wrap: wrap;
    gap: 0.75rem;
  }

  .pie-section h2 {
    font-size: 1.1rem;
    font-weight: 600;
    margin: 0;
  }

  .range-badge {
    font-weight: 400;
    color: var(--text-muted);
    font-size: 0.9rem;
  }

  .metric-selector {
    display: flex;
    gap: 0.5rem;
    flex-wrap: wrap;
  }

  .metric-btn {
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

  .metric-btn.active {
    opacity: 1;
    border-color: var(--metric-color);
    color: var(--metric-color);
  }

  .metric-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--metric-color);
  }

  .pie-empty {
    color: var(--text-muted);
    text-align: center;
    padding: 2rem 0;
    font-size: 0.9rem;
  }

  .pie-layout {
    display: flex;
    align-items: flex-start;
    gap: 2rem;
    flex-wrap: wrap;
    justify-content: center;
  }

  .pie-container {
    position: relative;
    flex-shrink: 0;
  }

  .pie-container.pie-loading svg {
    opacity: 0.35;
    transition: opacity 0.2s ease;
  }

  .pie-overlay {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 5;
    pointer-events: none;
  }

  .pie-spinner {
    width: 28px;
    height: 28px;
    border: 3px solid var(--border);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: pie-spin 0.8s linear infinite;
  }

  @keyframes pie-spin {
    to { transform: rotate(360deg); }
  }

  .pie-tooltip {
    position: absolute;
    transform: translate(-50%, -110%);
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 0.5rem;
    padding: 0.5rem 0.75rem;
    pointer-events: none;
    z-index: 10;
    white-space: nowrap;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.3);
  }

  .pie-tooltip-label {
    font-size: 0.85rem;
    font-weight: 600;
    margin-bottom: 0.15rem;
  }

  .pie-tooltip-value {
    font-size: 0.8rem;
    color: var(--text-muted);
  }

  .pie-legend {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    min-width: 180px;
  }

  .pie-legend-item {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: 0.8rem;
    cursor: default;
    transition: opacity 0.15s;
  }

  .pie-legend-item.dimmed {
    opacity: 0.4;
  }

  .pie-legend-item.clickable {
    cursor: pointer;
    user-select: none;
  }

  .pie-legend-item.clickable:hover .pie-legend-chevron {
    color: var(--text);
  }

  .pie-legend-chevron {
    color: var(--text-muted);
    font-size: 0.7rem;
    margin-left: 0.3rem;
  }

  .pie-legend-sublist {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    margin: 0.2rem 0 0.4rem 1rem;
    padding-left: 0.6rem;
    border-left: 2px solid var(--border);
  }

  .pie-legend-subitem {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: 0.75rem;
    color: var(--text-muted);
  }

  .pie-legend-subitem .pie-legend-label {
    flex: 1;
    white-space: nowrap;
  }

  .pie-legend-subitem .pie-legend-value {
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }

  .pie-legend-subitem .pie-legend-pct {
    font-variant-numeric: tabular-nums;
    min-width: 3rem;
    text-align: right;
  }

  .pie-legend-dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    flex-shrink: 0;
  }

  .pie-legend-label {
    flex: 1;
    white-space: nowrap;
  }

  .pie-legend-value {
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }

  .pie-legend-pct {
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
    min-width: 3rem;
    text-align: right;
  }

  @media (max-width: 600px) {
    .pie-layout {
      flex-direction: column;
      align-items: center;
    }

    .pie-legend {
      width: 100%;
    }
  }
</style>
