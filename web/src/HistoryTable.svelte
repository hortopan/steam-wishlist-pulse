<script lang="ts">
  import { api, AuthError } from "./api";
  import { formatSteamDate, formatTimePacific, countryFlag } from "./utils";
  import type {
    AnomalyMetrics,
    CountryEntry,
    DailyHistoryResponse,
    HistoryDayEntry,
    DayCountriesResponse,
  } from "./types";

  let {
    history,
    loading,
    flashDates,
    flashSnapshotIds,
    appId,
    onPageChange,
    onAuthError,
  }: {
    history: DailyHistoryResponse | null;
    loading: boolean;
    flashDates: Set<string>;
    flashSnapshotIds: Set<number>;
    appId: number;
    onPageChange: (page: number) => void;
    onAuthError: () => void;
  } = $props();

  const METRICS = ["adds", "deletes", "purchases", "gifts"] as const;

  // Expansion state is keyed by Steam date so it survives the 30s data refresh.
  let expandedDays = $state<Set<string>>(new Set());
  // snapshotId records which snapshot the data was fetched at, so the panel
  // refreshes when the in-progress day advances.
  let expandedCountries = $state<Map<string, { snapshotId: number; countries: CountryEntry[] }>>(new Map());
  let loadingCountries = $state<Set<string>>(new Set());

  function toggleDay(date: string) {
    const next = new Set(expandedDays);
    if (next.has(date)) {
      next.delete(date);
    } else {
      next.add(date);
    }
    expandedDays = next;
  }

  async function fetchCountries(day: HistoryDayEntry) {
    const nextLoading = new Set(loadingCountries);
    nextLoading.add(day.date);
    loadingCountries = nextLoading;

    try {
      const resp = await api<DayCountriesResponse>(
        `/wishlist/${appId}/countries/day/${day.date}`
      );
      const next = new Map(expandedCountries);
      next.set(day.date, { snapshotId: day.last_snapshot_id, countries: resp.countries });
      expandedCountries = next;
    } catch (e: any) {
      if (e instanceof AuthError) { onAuthError(); return; }
    } finally {
      const nextLoading = new Set(loadingCountries);
      nextLoading.delete(day.date);
      loadingCountries = nextLoading;
    }
  }

  function toggleCountries(day: HistoryDayEntry) {
    if (expandedCountries.has(day.date)) {
      const next = new Map(expandedCountries);
      next.delete(day.date);
      expandedCountries = next;
      return;
    }
    fetchCountries(day);
  }

  $effect(() => {
    if (!history) return;
    for (const day of history.days) {
      const cached = expandedCountries.get(day.date);
      if (cached && cached.snapshotId !== day.last_snapshot_id && !loadingCountries.has(day.date)) {
        fetchCountries(day);
      }
    }
  });

  // Anomaly popover state
  let anomalyPopover = $state<{
    x: number;
    y: number;
    metric: string;
    desc: string;
  } | null>(null);
  let anomalyPopoverTimer: ReturnType<typeof setTimeout> | null = null;

  function showAnomalyPopover(e: MouseEvent, am: AnomalyMetrics, metric: string) {
    if (anomalyPopoverTimer) clearTimeout(anomalyPopoverTimer);
    const rect = (e.target as HTMLElement).getBoundingClientRect();
    const desc = (am?.descriptions ?? []).find(d => d.toLowerCase().startsWith(metric)) ?? 'Anomalous change detected';
    anomalyPopover = { x: rect.left + rect.width / 2, y: rect.top, metric, desc };
  }

  function hideAnomalyPopover() {
    anomalyPopoverTimer = setTimeout(() => { anomalyPopover = null; }, 150);
  }

  function toggleAnomalyPopover(e: MouseEvent, am: AnomalyMetrics, metric: string) {
    e.stopPropagation();
    if (anomalyPopover && anomalyPopover.metric === metric) {
      anomalyPopover = null;
    } else {
      showAnomalyPopover(e, am, metric);
    }
  }

  function fmtDelta(n: number): string {
    return n > 0 ? `+${n.toLocaleString()}` : n.toLocaleString();
  }

  let totalPages = $derived(
    history ? Math.max(1, Math.ceil(history.total_days / history.per_page)) : 1,
  );
  let firstDayIndex = $derived(history ? (history.page - 1) * history.per_page + 1 : 0);
  let lastDayIndex = $derived(
    history ? Math.min(history.page * history.per_page, history.total_days) : 0,
  );
</script>

{#if history && history.days.length > 0}
  <div class="history-section">
    <h2>Daily History <span class="muted-count">({history.total_days} days &middot; times in PT)</span></h2>
    <div class="history-table-wrap">
      <table class="history-table">
        <thead>
          <tr>
            <th class="day-col">Day (PT)</th>
            <th class="num">Adds</th>
            <th class="num">Deletes</th>
            <th class="num">Wishlist Conversions</th>
            <th class="num">Gifts</th>
            <th class="num platform-col">Win</th>
            <th class="num platform-col">Mac</th>
            <th class="num platform-col">Linux</th>
            <th class="num">Countries</th>
          </tr>
        </thead>
        <tbody>
          {#each history.days as day (day.date)}
            {@const expanded = expandedDays.has(day.date)}
            <tr
              class="day-row"
              class:expandable={!day.is_daily_report}
              class:flash-row={flashDates.has(day.date)}
              onclick={() => { if (!day.is_daily_report) toggleDay(day.date); }}
            >
              <td class="day-cell">
                {#if day.is_daily_report}
                  <span class="day-toggle-spacer"></span>
                {:else}
                  <button
                    type="button"
                    class="day-toggle"
                    aria-expanded={expanded}
                    aria-label={expanded ? "Collapse day" : "Expand day"}
                    onclick={(e) => { e.stopPropagation(); toggleDay(day.date); }}
                  >{expanded ? "▾" : "▸"}</button>
                {/if}
                <span class="day-date">{formatSteamDate(day.date)}</span>
                {#if day.is_daily_report}
                  <span class="count-badge">daily report</span>
                {:else if day.snapshot_count > 1}
                  <span class="count-badge">×{day.snapshot_count}</span>
                {/if}
              </td>
              {#each METRICS as metric}
                {@const am = day.anomaly_metrics}
                {@const isAnomaly = !!(am as any)?.[metric]}
                {@const desc = isAnomaly ? (am?.descriptions ?? []).find(d => d.toLowerCase().startsWith(metric)) : null}
                {@const isUp = desc ? /above|spike/i.test(desc) : false}
                <td class="num {metric}">
                  {((day as any)[metric] as number).toLocaleString()}
                  {#if isAnomaly}
                    <button
                      type="button"
                      class="anomaly-arrow"
                      class:up={isUp}
                      class:down={!isUp}
                      onclick={(e: MouseEvent) => toggleAnomalyPopover(e, am, metric)}
                      onmouseenter={(e: MouseEvent) => showAnomalyPopover(e, am, metric)}
                      onmouseleave={() => hideAnomalyPopover()}
                      title={desc ?? 'Anomalous change detected'}
                    >{isUp ? '▲' : '▼'}</button>
                  {/if}
                </td>
              {/each}
              <td class="num platform-val">{day.adds_windows.toLocaleString()}</td>
              <td class="num platform-val">{day.adds_mac.toLocaleString()}</td>
              <td class="num platform-val">{day.adds_linux.toLocaleString()}</td>
              <td class="num">
                <button
                  class="country-expand-btn"
                  class:loading-spin={loadingCountries.has(day.date)}
                  onclick={(e) => { e.stopPropagation(); toggleCountries(day); }}
                  title={expandedCountries.has(day.date) ? "Hide countries" : "Show countries"}
                >
                  {#if loadingCountries.has(day.date)}
                    <span class="mini-spinner"></span>
                  {:else if expandedCountries.has(day.date)}
                    ▲
                  {:else}
                    ▼
                  {/if}
                </button>
              </td>
            </tr>
            {#if expandedCountries.has(day.date)}
              {@const countries = expandedCountries.get(day.date)?.countries ?? []}
              {@const sorted = [...countries].sort((a, b) => b.adds - a.adds)}
              <tr class="country-detail-row">
                <td colspan="9">
                  <div class="country-detail-grid">
                    {#each sorted.slice(0, 20) as c}
                      <div class="country-mini">
                        <span class="country-flag">{countryFlag(c.country_code)}</span>
                        <span class="country-code">{c.country_code}</span>
                        <span class="country-val adds">+{c.adds}</span>
                        <span class="country-val deletes">-{c.deletes}</span>
                      </div>
                    {/each}
                    {#if countries.length > 20}
                      <div class="country-mini muted">+{countries.length - 20} more</div>
                    {/if}
                  </div>
                </td>
              </tr>
            {/if}
            {#if expanded}
              {#each day.snapshots.filter(s => !s.is_eod_report) as s (s.snapshot_id)}
                <tr class="snap-row" class:flash-row={flashSnapshotIds.has(s.snapshot_id)}>
                  <td class="snap-time">{formatTimePacific(s.fetched_at)}</td>
                  <td class="num adds" class:delta-dim={s.delta_adds <= 0}>{fmtDelta(s.delta_adds)}</td>
                  <td class="num deletes" class:delta-dim={s.delta_deletes <= 0}>{fmtDelta(s.delta_deletes)}</td>
                  <td class="num purchases" class:delta-dim={s.delta_purchases <= 0}>{fmtDelta(s.delta_purchases)}</td>
                  <td class="num gifts" class:delta-dim={s.delta_gifts <= 0}>{fmtDelta(s.delta_gifts)}</td>
                  <td class="num platform-val">{fmtDelta(s.delta_adds_windows)}</td>
                  <td class="num platform-val">{fmtDelta(s.delta_adds_mac)}</td>
                  <td class="num platform-val">{fmtDelta(s.delta_adds_linux)}</td>
                  <td></td>
                </tr>
              {/each}
            {/if}
          {/each}
        </tbody>
      </table>
    </div>
    {#if anomalyPopover}
      <div
        class="anomaly-popover"
        style="left: {anomalyPopover.x}px; top: {anomalyPopover.y}px;"
      >
        {anomalyPopover.desc}
      </div>
    {/if}
    <!-- Pagination controls -->
    <div class="pagination-row">
      <button
        class="pagination-btn"
        disabled={history.page <= 1 || loading}
        onclick={() => onPageChange(history!.page - 1)}
      >
        &larr; Newer
      </button>
      <span class="pagination-info">
        Days {firstDayIndex}–{lastDayIndex} of {history.total_days} (page {history.page} of {totalPages})
      </span>
      <button
        class="pagination-btn"
        disabled={history.page >= totalPages || loading}
        onclick={() => onPageChange(history!.page + 1)}
      >
        Older &rarr;
      </button>
    </div>
  </div>
{/if}

<style>
  .history-section {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 0.75rem;
    padding: 1.5rem;
    margin-bottom: 1.5rem;
  }

  .history-section h2 {
    font-size: 1.1rem;
    font-weight: 600;
    margin-bottom: 1rem;
  }

  .muted-count {
    font-size: 0.8rem;
    font-weight: 400;
    color: var(--text-muted);
  }

  .history-table-wrap {
    overflow-x: auto;
  }

  .history-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.85rem;
  }

  .history-table th {
    text-align: left;
    padding: 0.6rem 0.75rem;
    border-bottom: 1px solid var(--border);
    color: var(--text-muted);
    font-weight: 500;
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .history-table td {
    padding: 0.6rem 0.75rem;
    border-bottom: 1px solid rgba(255, 255, 255, 0.04);
    font-variant-numeric: tabular-nums;
  }

  .history-table th.num,
  .history-table td.num {
    text-align: right;
  }

  .history-table td.adds {
    color: var(--green);
  }
  .history-table td.deletes {
    color: var(--red);
  }
  .history-table td.purchases {
    color: var(--blue);
  }
  .history-table td.gifts {
    color: var(--amber);
  }
  .history-table td.delta-dim {
    color: var(--text-muted);
  }

  /* Platform columns */
  .platform-col {
    color: var(--text-muted);
    font-size: 0.7rem;
  }

  .platform-val {
    color: var(--text-muted);
    font-size: 0.8rem;
  }

  /* Day rows */
  .day-row.expandable {
    cursor: pointer;
  }

  /* Fixed slots: column width and badge positions stay identical across
     rows and pages regardless of weekday width or badge presence */
  .history-table th.day-col {
    width: 14rem;
  }

  .day-cell {
    white-space: nowrap;
  }

  .day-date {
    display: inline-block;
    min-width: 7.6rem;
  }

  .day-toggle {
    background: none;
    border: none;
    color: var(--text-muted);
    cursor: pointer;
    font-size: 0.7rem;
    padding: 0;
    line-height: 1;
    width: 1.1rem;
    text-align: left;
  }

  .day-toggle-spacer {
    display: inline-block;
    width: 1.1rem;
  }

  .count-badge {
    color: var(--text-muted);
    font-size: 0.7rem;
    margin-left: 0.35rem;
  }

  /* Intra-day snapshot rows */
  .snap-row td {
    background: rgba(99, 102, 241, 0.03);
    border-bottom: 1px solid rgba(255, 255, 255, 0.03);
  }

  .history-table td.snap-time {
    color: var(--text-muted);
    font-size: 0.8rem;
    padding-left: 1.9rem;
    white-space: nowrap;
  }

  .anomaly-arrow {
    display: inline-block;
    font-size: 0.6rem;
    margin-left: 0.25rem;
    cursor: pointer;
    vertical-align: middle;
    background: none;
    border: none;
    padding: 0;
    font: inherit;
    line-height: 1;
  }

  .anomaly-arrow.up {
    color: var(--green);
  }

  .anomaly-arrow.down {
    color: var(--red);
  }

  .anomaly-popover {
    position: fixed;
    transform: translate(-50%, -100%) translateY(-8px);
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 0.5rem;
    padding: 0.4rem 0.65rem;
    font-size: 0.75rem;
    color: var(--text);
    white-space: nowrap;
    z-index: 100;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);
    pointer-events: none;
    line-height: 1.4;
  }

  .history-table tbody tr:hover {
    background: rgba(255, 255, 255, 0.02);
  }

  .history-table tbody tr.flash-row {
    animation: flash-row 1.5s ease-out;
  }

  .history-table tbody tr.flash-row td {
    animation: slide-in 0.4s ease-out;
  }

  @keyframes flash-row {
    0% {
      background: rgba(99, 102, 241, 0.25);
    }
    40% {
      background: rgba(99, 102, 241, 0.1);
    }
    100% {
      background: transparent;
    }
  }

  @keyframes slide-in {
    0% {
      opacity: 0;
      transform: translateY(-8px);
    }
    100% {
      opacity: 1;
      transform: translateY(0);
    }
  }

  /* Country expand button */
  .country-expand-btn {
    background: none;
    border: 1px solid var(--border);
    color: var(--text-muted);
    padding: 0.2rem 0.5rem;
    border-radius: 0.25rem;
    cursor: pointer;
    font-size: 0.7rem;
    transition: border-color 0.2s, color 0.2s;
  }

  .country-expand-btn:hover {
    border-color: var(--accent);
    color: var(--accent);
  }

  .mini-spinner {
    display: inline-block;
    width: 0.7rem;
    height: 0.7rem;
    border: 2px solid var(--border);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.6s linear infinite;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  /* Inline country detail */
  .country-detail-row td {
    padding: 0.5rem 0.75rem;
    background: rgba(99, 102, 241, 0.03);
  }

  .country-detail-grid {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
  }

  .country-mini {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    font-size: 0.75rem;
    background: var(--surface);
    border: 1px solid var(--border);
    padding: 0.2rem 0.5rem;
    border-radius: 0.25rem;
  }

  .country-mini.muted {
    color: var(--text-muted);
  }

  .country-flag {
    font-size: 1.1em;
  }

  .country-code {
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .country-val.adds {
    color: var(--green);
  }

  .country-val.deletes {
    color: var(--red);
  }

  /* Pagination */
  .pagination-row {
    display: flex;
    justify-content: center;
    align-items: center;
    gap: 1rem;
    margin-top: 1rem;
    padding-top: 0.75rem;
  }

  .pagination-btn {
    background: rgba(99, 102, 241, 0.1);
    border: 1px solid var(--accent);
    border-radius: 0.5rem;
    color: var(--accent);
    font-size: 0.85rem;
    font-weight: 500;
    cursor: pointer;
    padding: 0.5rem 1rem;
    transition: background 0.2s, color 0.2s;
  }

  .pagination-btn:hover:not(:disabled) {
    background: rgba(99, 102, 241, 0.2);
    color: var(--text);
  }

  .pagination-btn:disabled {
    opacity: 0.3;
    cursor: not-allowed;
  }

  .pagination-info {
    font-size: 0.8rem;
    color: var(--text-muted);
  }

  /* Responsive */
  @media (max-width: 640px) {
    .history-section {
      padding: 1rem;
    }

    .history-table th.day-col {
      width: auto;
    }

    .history-table {
      font-size: 0.78rem;
    }

    .history-table th,
    .history-table td {
      padding: 0.5rem 0.4rem;
    }
  }
</style>
