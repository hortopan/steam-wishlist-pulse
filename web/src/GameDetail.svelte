<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { playNotificationSound } from "./notificationSound";
  import { api, AuthError } from "./api";
  import { timeAgo } from "./utils";
  import { POLL_INTERVAL, TICK_INTERVAL, FLASH_DURATION, FLASH_ROW_DURATION, METRIC_KEYS } from "./constants";
  import type {
    CountryEntry,
    GameReport,
    GameDetailResponse,
    ChartResponse,
    ChartPoint,
    PaginatedHistoryResponse,
    HistoryEntry,
    SnapshotCountriesResponse,
  } from "./types";
  import Chart from "./Chart.svelte";

  function countryFlag(code: string): string {
    if (!code || !/^[a-zA-Z]{2}$/.test(code)) return "";
    return [...code.toUpperCase()].map(c => String.fromCodePoint(0x1F1E6 + c.charCodeAt(0) - 65)).join("");
  }

  let {
    appId,
    onBack,
    onLogout,
  }: {
    appId: number;
    onBack: () => void;
    onLogout: () => void;
  } = $props();

  // ── State ──────────────────────────────────────────────────────
  let detail = $state<GameDetailResponse | null>(null);
  let chartData = $state<ChartResponse | null>(null);
  let historyData = $state<PaginatedHistoryResponse | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let now = $state(Date.now());
  let pollTimer: ReturnType<typeof setTimeout> | null = null;
  let tickTimer: ReturnType<typeof setInterval> | null = null;
  let abortController: AbortController | null = null;
  let chartAbortController: AbortController | null = null;
  let destroyed = false;

  // Chart range
  type ChartRange = "1d" | "2d" | "3d" | "7d" | "1m" | "3m" | "1y" | "5y" | "all";
  const CHART_RANGES: { key: ChartRange; label: string }[] = [
    { key: "1d", label: "1D" },
    { key: "2d", label: "2D" },
    { key: "3d", label: "3D" },
    { key: "7d", label: "7D" },
    { key: "1m", label: "1M" },
    { key: "3m", label: "3M" },
    { key: "1y", label: "1Y" },
    { key: "5y", label: "5Y" },
    { key: "all", label: "All" },
  ];
  let chartRange = $state<ChartRange>("7d");

  // History pagination
  let historyPage = $state(1);
  const HISTORY_PAGE_SIZE = 24;
  let historyLoading = $state(false);
  let chartLoading = $state(false);

  // Country lazy-loading
  let expandedCountries = $state<Map<number, CountryEntry[]>>(new Map());
  let loadingCountries = $state<Set<number>>(new Set());

  // Animation: track which stats changed on last poll
  let flashMetrics = $state<Set<string>>(new Set());
  let flashRows = $state<Set<string>>(new Set());
  let prevLatest: GameReport | null = null;
  let prevHistoryIds = new Set<number>();

  function schedulePoll() {
    pollTimer = setTimeout(async () => {
      await fetchDetail();
      schedulePoll();
    }, POLL_INTERVAL);
  }

  // ── Fetch functions ────────────────────────────────────────────

  async function fetchDetail() {
    if (!detail) loading = true;
    error = null;
    abortController?.abort();
    abortController = new AbortController();
    const { signal } = abortController;
    try {
      const newDetail = await api<GameDetailResponse>(`/wishlist/${appId}/detail`, { signal });
      if (destroyed) return;

      // Detect changed metrics for flash animation
      if (prevLatest && newDetail.latest) {
        const changed = new Set<string>();
        for (const key of METRIC_KEYS) {
          if ((newDetail.latest as any)[key] !== (prevLatest as any)[key]) {
            changed.add(key);
          }
        }
        if (changed.size > 0) {
          flashMetrics = changed;
          playNotificationSound();
          setTimeout(() => (flashMetrics = new Set()), FLASH_DURATION);
        }
      }

      if (newDetail.latest) prevLatest = { ...newDetail.latest };
      detail = newDetail;

      // Refresh chart and first page of history on each poll
      chartAbortController?.abort();
      chartAbortController = new AbortController();
      await Promise.all([
        fetchChart(chartRange, chartAbortController.signal),
        historyPage === 1 ? fetchHistory(1, signal) : Promise.resolve(),
      ]);
    } catch (e: any) {
      if (signal.aborted) return;
      if (e instanceof AuthError) { onLogout(); return; }
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function fetchChart(range: ChartRange, signal?: AbortSignal) {
    chartLoading = true;
    try {
      const data = await api<ChartResponse>(`/wishlist/${appId}/chart?range=${range}`, { signal });
      if (destroyed) return;
      // Only apply if the range still matches what the user selected
      if (range === chartRange) chartData = data;
    } catch (e: any) {
      if (signal?.aborted) return;
      if (e instanceof AuthError) { onLogout(); return; }
      // Chart errors are non-fatal, keep old data
    } finally {
      chartLoading = false;
    }
  }

  async function fetchHistory(page: number, signal?: AbortSignal) {
    historyLoading = true;
    try {
      const newHistory = await api<PaginatedHistoryResponse>(
        `/wishlist/${appId}/history?page=${page}&per_page=${HISTORY_PAGE_SIZE}`,
        { signal },
      );
      if (destroyed) return;

      // Detect new rows for flash animation
      if (page === 1 && historyData) {
        const newDates = new Set<string>();
        for (const entry of newHistory.entries) {
          if (!prevHistoryIds.has(entry.snapshot_id)) {
            newDates.add(entry.date);
          }
        }
        if (newDates.size > 0) {
          flashRows = newDates;
          setTimeout(() => (flashRows = new Set()), FLASH_ROW_DURATION);
        }
      }

      prevHistoryIds = new Set(newHistory.entries.map(e => e.snapshot_id));
      historyData = newHistory;
      historyPage = page;
    } catch (e: any) {
      if (signal?.aborted) return;
      if (e instanceof AuthError) { onLogout(); return; }
      error = e.message;
    } finally {
      historyLoading = false;
    }
  }

  async function loadCountries(snapshotId: number) {
    if (expandedCountries.has(snapshotId)) {
      // Toggle off
      const next = new Map(expandedCountries);
      next.delete(snapshotId);
      expandedCountries = next;
      return;
    }

    const nextLoading = new Set(loadingCountries);
    nextLoading.add(snapshotId);
    loadingCountries = nextLoading;

    try {
      const resp = await api<SnapshotCountriesResponse>(
        `/wishlist/${appId}/countries/${snapshotId}`
      );
      const next = new Map(expandedCountries);
      next.set(snapshotId, resp.countries);
      expandedCountries = next;
    } catch (e: any) {
      if (e instanceof AuthError) { onLogout(); return; }
    } finally {
      const nextLoading = new Set(loadingCountries);
      nextLoading.delete(snapshotId);
      loadingCountries = nextLoading;
    }
  }

  async function changeChartRange(range: ChartRange) {
    chartRange = range;
    chartAbortController?.abort();
    chartAbortController = new AbortController();
    await fetchChart(range, chartAbortController.signal);
  }

  onMount(() => {
    fetchDetail();
    schedulePoll();
    tickTimer = setInterval(() => {
      now = Date.now();
    }, TICK_INTERVAL);
  });

  onDestroy(() => {
    destroyed = true;
    abortController?.abort();
    chartAbortController?.abort();
    if (pollTimer) clearTimeout(pollTimer);
    if (tickTimer) clearInterval(tickTimer);
  });
</script>

<div class="game-detail">
  <button class="back-btn" onclick={onBack}>
    <svg width="20" height="20" viewBox="0 0 20 20" fill="none">
      <path
        d="M12.5 15L7.5 10L12.5 5"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
      />
    </svg>
    Back to Dashboard
  </button>

  {#if error}
    <div class="error-banner">
      <span>Error: {error}</span>
      <button onclick={fetchDetail}>Retry</button>
    </div>
  {/if}

  {#if loading && !detail}
    <div class="loading">
      <div class="spinner"></div>
    </div>
  {:else if detail}
    <!-- Hero Section -->
    <div class="hero">
      {#if detail.image_url}
        <img class="hero-image" src={detail.image_url} alt={detail.name} />
      {/if}
      <div class="hero-overlay">
        <h1 class="hero-title">{detail.name}</h1>
        <span class="hero-appid">App ID: {detail.app_id}</span>
        {#if detail.latest?.changed_at}
          <span class="hero-updated"
            >Last updated {timeAgo(detail.latest.changed_at, now)}</span
          >
        {/if}
        <div class="hero-links">
          <a href={`https://store.steampowered.com/app/${detail.app_id}`} target="_blank" rel="noopener noreferrer" class="hero-link">
            <svg width="14" height="14" viewBox="0 0 20 20" fill="none"><path d="M11 3H17V9" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/><path d="M17 3L9 11" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/><path d="M15 11V16C15 16.5523 14.5523 17 14 17H4C3.44772 17 3 16.5523 3 16V6C3 5.44772 3.44772 5 4 5H9" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>
            Store Page
          </a>
          <a href={`https://partner.steamgames.com/apps/landing/${detail.app_id}`} target="_blank" rel="noopener noreferrer" class="hero-link">
            <svg width="14" height="14" viewBox="0 0 20 20" fill="none"><path d="M11 3H17V9" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/><path d="M17 3L9 11" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/><path d="M15 11V16C15 16.5523 14.5523 17 14 17H4C3.44772 17 3 16.5523 3 16V6C3 5.44772 3.44772 5 4 5H9" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>
            Steamworks
          </a>
        </div>
      </div>
    </div>

    <!-- Stats Cards -->
    {#if detail.latest}
      <div class="stats-row">
        <div class="stat-card stat-adds" class:flash={flashMetrics.has("adds")}>
          <div class="stat-section-today">
            <div class="stat-period-label">Today</div>
            <div class="stat-big-value">{detail.latest.adds.toLocaleString()}</div>
          </div>
          <div class="stat-section-total">
            <div class="stat-period-label">All-Time</div>
            <div class="stat-total-value">{detail.latest.total_adds.toLocaleString()}</div>
          </div>
          <div class="stat-big-label">Wishlist Adds</div>
        </div>
        <div
          class="stat-card stat-deletes"
          class:flash={flashMetrics.has("deletes")}
        >
          <div class="stat-section-today">
            <div class="stat-period-label">Today</div>
            <div class="stat-big-value">{detail.latest.deletes.toLocaleString()}</div>
          </div>
          <div class="stat-section-total">
            <div class="stat-period-label">All-Time</div>
            <div class="stat-total-value">{detail.latest.total_deletes.toLocaleString()}</div>
          </div>
          <div class="stat-big-label">Wishlist Deletes</div>
        </div>
        <div
          class="stat-card stat-purchases"
          class:flash={flashMetrics.has("purchases")}
        >
          <div class="stat-section-today">
            <div class="stat-period-label">Today</div>
            <div class="stat-big-value">{detail.latest.purchases.toLocaleString()}</div>
          </div>
          <div class="stat-section-total">
            <div class="stat-period-label">All-Time</div>
            <div class="stat-total-value">{detail.latest.total_purchases.toLocaleString()}</div>
          </div>
          <div class="stat-big-label">Purchases</div>
        </div>
        <div
          class="stat-card stat-gifts"
          class:flash={flashMetrics.has("gifts")}
        >
          <div class="stat-section-today">
            <div class="stat-period-label">Today</div>
            <div class="stat-big-value">{detail.latest.gifts.toLocaleString()}</div>
          </div>
          <div class="stat-section-total">
            <div class="stat-period-label">All-Time</div>
            <div class="stat-total-value">{detail.latest.total_gifts.toLocaleString()}</div>
          </div>
          <div class="stat-big-label">Gifts</div>
        </div>
      </div>

      <!-- Platform Breakdown -->
      {@const totalPlatform = detail.latest.adds_windows + detail.latest.adds_mac + detail.latest.adds_linux}
      {@const pctWin = totalPlatform ? (detail.latest.adds_windows / totalPlatform) * 100 : 0}
      {@const pctMac = totalPlatform ? (detail.latest.adds_mac / totalPlatform) * 100 : 0}
      {@const pctLinux = totalPlatform ? (detail.latest.adds_linux / totalPlatform) * 100 : 0}
      {#if totalPlatform > 0}
        <div class="platform-section">
          <h3 class="section-subtitle">Adds by Platform</h3>
          <div class="platform-bars">
            <div class="platform-bar-track">
              {#if pctWin > 0}<div class="platform-segment seg-windows" style="width:{pctWin}%"></div>{/if}
              {#if pctMac > 0}<div class="platform-segment seg-mac" style="width:{pctMac}%"></div>{/if}
              {#if pctLinux > 0}<div class="platform-segment seg-linux" style="width:{pctLinux}%"></div>{/if}
            </div>
            <div class="platform-legend">
              <span class="legend-item"><span class="legend-dot dot-windows"></span> Windows {detail.latest.adds_windows.toLocaleString()} ({pctWin.toFixed(1)}%)</span>
              <span class="legend-item"><span class="legend-dot dot-mac"></span> macOS {detail.latest.adds_mac.toLocaleString()} ({pctMac.toFixed(1)}%)</span>
              <span class="legend-item"><span class="legend-dot dot-linux"></span> Linux {detail.latest.adds_linux.toLocaleString()} ({pctLinux.toFixed(1)}%)</span>
            </div>
          </div>
        </div>
      {/if}

      <!-- Net Change -->
      {@const net = detail.latest.adds - detail.latest.deletes}
      <div class="net-row" class:flash-net={flashMetrics.size > 0}>
        <span class="net-label">Net Wishlist Change Today</span>
        <span
          class="net-value"
          class:positive={net > 0}
          class:negative={net < 0}
        >
          {net > 0 ? "+" : ""}{net.toLocaleString()}
        </span>
      </div>
    {:else}
      <div class="no-data-banner">
        No data available yet. Stats will appear after the first poll.
      </div>
    {/if}

    <!-- Chart with range selector -->
    {#if chartData}
      <Chart history={chartData.points} resolution={chartData.resolution} {chartRange} onRangeChange={changeChartRange} ranges={CHART_RANGES} loading={chartLoading} />
    {/if}

    <!-- Top Countries (latest snapshot) -->
    {#if detail.latest && detail.latest.countries.length > 0}
      {@const sortedCountries = [...detail.latest.countries].sort((a, b) => b.adds - a.adds)}
      <div class="countries-section">
        <h2>Top Countries for today <span class="muted-count">({detail.latest.countries.length} total)</span></h2>
        <div class="countries-table-wrap">
          <table class="history-table">
            <thead>
              <tr>
                <th>Country</th>
                <th class="num">Adds</th>
                <th class="num">Deletes</th>
                <th class="num">Purchases</th>
                <th class="num">Gifts</th>
              </tr>
            </thead>
            <tbody>
              {#each sortedCountries.slice(0, 20) as country}
                <tr>
                  <td class="country-cell"><span class="country-flag">{countryFlag(country.country_code)}</span> {country.country_code}</td>
                  <td class="num adds">{country.adds.toLocaleString()}</td>
                  <td class="num deletes">{country.deletes.toLocaleString()}</td>
                  <td class="num purchases">{country.purchases.toLocaleString()}</td>
                  <td class="num gifts">{country.gifts.toLocaleString()}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      </div>
    {/if}

    <!-- Snapshot History Table (server-paginated) -->
    {#if historyData && historyData.entries.length > 0}
      <div class="history-section">
        <h2>Snapshot History <span class="muted-count">({historyData.total} total, page {historyData.page})</span></h2>
        <div class="history-table-wrap">
          <table class="history-table">
            <thead>
              <tr>
                <th>Date</th>
                <th class="num">Adds</th>
                <th class="num">Deletes</th>
                <th class="num">Purchases</th>
                <th class="num">Gifts</th>
                <th class="num platform-col">Win</th>
                <th class="num platform-col">Mac</th>
                <th class="num platform-col">Linux</th>
                <th>Recorded</th>
                <th class="num">Countries</th>
              </tr>
            </thead>
            <tbody>
              {#each historyData.entries as entry}
                <tr class:flash-row={flashRows.has(entry.date)} class:anomaly-row={entry.is_anomaly}>
                  <td>
                    {#if entry.is_anomaly}<span class="anomaly-badge" title="Anomalous change detected">&#9888;</span>{/if}
                    {entry.date.split("T")[0]}
                    {#if entry.is_anomaly && entry.anomaly_metrics?.descriptions?.length}
                      <div class="anomaly-detail">
                        {#each entry.anomaly_metrics.descriptions as desc}
                          <div class="anomaly-detail-line">{desc}</div>
                        {/each}
                      </div>
                    {/if}
                  </td>
                  <td class="num adds">{entry.adds.toLocaleString()}</td>
                  <td class="num deletes">{entry.deletes.toLocaleString()}</td>
                  <td class="num purchases">{entry.purchases.toLocaleString()}</td>
                  <td class="num gifts">{entry.gifts.toLocaleString()}</td>
                  <td class="num platform-val">{entry.adds_windows.toLocaleString()}</td>
                  <td class="num platform-val">{entry.adds_mac.toLocaleString()}</td>
                  <td class="num platform-val">{entry.adds_linux.toLocaleString()}</td>
                  <td class="muted"
                    >{entry.fetched_at
                      ? timeAgo(entry.fetched_at, now)
                      : "—"}</td
                  >
                  <td class="num">
                    <button
                      class="country-expand-btn"
                      class:loading-spin={loadingCountries.has(entry.snapshot_id)}
                      onclick={() => loadCountries(entry.snapshot_id)}
                      title={expandedCountries.has(entry.snapshot_id) ? "Hide countries" : "Show countries"}
                    >
                      {#if loadingCountries.has(entry.snapshot_id)}
                        <span class="mini-spinner"></span>
                      {:else if expandedCountries.has(entry.snapshot_id)}
                        ▲
                      {:else}
                        ▼
                      {/if}
                    </button>
                  </td>
                </tr>
                {#if expandedCountries.has(entry.snapshot_id)}
                  {@const countries = expandedCountries.get(entry.snapshot_id) ?? []}
                  {@const sorted = [...countries].sort((a, b) => b.adds - a.adds)}
                  <tr class="country-detail-row">
                    <td colspan="10">
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
              {/each}
            </tbody>
          </table>
        </div>
        <!-- Pagination controls -->
        <div class="pagination-row">
          <button
            class="pagination-btn"
            disabled={historyPage <= 1 || historyLoading}
            onclick={() => fetchHistory(historyPage - 1)}
          >
            &larr; Newer
          </button>
          <span class="pagination-info">
            Page {historyData.page} of {Math.ceil(historyData.total / historyData.per_page)}
          </span>
          <button
            class="pagination-btn"
            disabled={historyPage * historyData.per_page >= historyData.total || historyLoading}
            onclick={() => fetchHistory(historyPage + 1)}
          >
            Older &rarr;
          </button>
        </div>
      </div>
    {/if}
  {/if}
</div>

<style>
  .game-detail {
    max-width: 900px;
    margin: 0 auto;
  }

  .back-btn {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    background: none;
    border: 1px solid var(--border);
    color: var(--text-muted);
    padding: 0.5rem 1rem;
    border-radius: 0.5rem;
    cursor: pointer;
    font-size: 0.85rem;
    margin-bottom: 1.5rem;
    transition:
      border-color 0.2s,
      color 0.2s;
  }

  .back-btn:hover {
    border-color: var(--accent);
    color: var(--text);
  }

  .error-banner {
    background: rgba(239, 68, 68, 0.1);
    border: 1px solid var(--red);
    border-radius: 0.5rem;
    padding: 1rem;
    margin-bottom: 1.5rem;
    display: flex;
    justify-content: space-between;
    align-items: center;
    color: var(--red);
  }

  .error-banner button {
    background: var(--red);
    color: white;
    border: none;
    padding: 0.4rem 0.8rem;
    border-radius: 0.375rem;
    cursor: pointer;
  }

  .loading {
    display: flex;
    justify-content: center;
    padding: 4rem 2rem;
  }

  .spinner {
    width: 2rem;
    height: 2rem;
    border: 3px solid var(--border);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  /* Hero */
  .hero {
    position: relative;
    border-radius: 0.75rem;
    overflow: hidden;
    margin-bottom: 1.5rem;
    border: 1px solid var(--border);
  }

  .hero-image {
    width: 100%;
    display: block;
  }

  .hero-overlay {
    position: absolute;
    bottom: 0;
    left: 0;
    right: 0;
    padding: 2rem 1.5rem 1.25rem;
    background: linear-gradient(transparent, rgba(0, 0, 0, 0.85));
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }

  .hero-title {
    font-size: 1.75rem;
    font-weight: 700;
    line-height: 1.2;
    text-shadow: 0 2px 8px rgba(0, 0, 0, 0.5);
  }

  .hero-appid {
    font-size: 0.8rem;
    color: var(--text-muted);
  }

  .hero-updated {
    font-size: 0.8rem;
    color: var(--accent);
  }

  .hero-links {
    display: flex;
    gap: 0.5rem;
    margin-top: 0.5rem;
    flex-wrap: wrap;
  }

  .hero-link {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.78rem;
    color: var(--text-muted);
    background: rgba(255, 255, 255, 0.1);
    padding: 0.3rem 0.65rem;
    border-radius: 0.375rem;
    text-decoration: none;
    transition: background 0.2s, color 0.2s;
  }

  .hero-link:hover {
    background: rgba(255, 255, 255, 0.2);
    color: var(--text);
  }

  /* Stats */
  .stats-row {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 0.75rem;
    margin-bottom: 1rem;
  }

  .stat-card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 0.75rem;
    padding: 1.25rem 1rem;
    text-align: center;
    transition:
      border-color 0.3s,
      box-shadow 0.3s;
  }

  .stat-card.flash {
    animation: pulse-card 1.2s ease-out;
  }

  .stat-card.flash .stat-big-value {
    animation: value-pop 0.6s cubic-bezier(0.34, 1.56, 0.64, 1);
  }

  @keyframes pulse-card {
    0% {
      border-color: var(--accent);
      box-shadow: 0 0 20px rgba(99, 102, 241, 0.35);
      transform: scale(1.04);
    }
    50% {
      border-color: var(--accent);
      box-shadow: 0 0 8px rgba(99, 102, 241, 0.15);
    }
    100% {
      border-color: var(--border);
      box-shadow: none;
      transform: scale(1);
    }
  }

  @keyframes value-pop {
    0% {
      transform: scale(1.3);
      opacity: 0.6;
    }
    100% {
      transform: scale(1);
      opacity: 1;
    }
  }

  .stat-big-value {
    font-size: 1.6rem;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    line-height: 1.2;
  }

  .stat-big-label {
    font-size: 0.7rem;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    margin-top: 0.75rem;
    padding-top: 0.5rem;
    border-top: 1px solid rgba(255, 255, 255, 0.06);
    text-align: center;
  }

  .stat-section-today {
    margin-bottom: 0.5rem;
  }

  .stat-section-total {
    opacity: 0.6;
  }

  .stat-period-label {
    font-size: 0.6rem;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.08em;
    margin-bottom: 0.15rem;
  }

  .stat-total-value {
    font-size: 1rem;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    line-height: 1.2;
    color: var(--text-secondary);
  }

  .stat-adds .stat-big-value {
    color: var(--green);
  }
  .stat-deletes .stat-big-value {
    color: var(--red);
  }
  .stat-purchases .stat-big-value {
    color: var(--blue);
  }
  .stat-gifts .stat-big-value {
    color: var(--amber);
  }

  /* Net change */
  .net-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 0.75rem;
    padding: 1rem 1.25rem;
    margin-bottom: 1.5rem;
  }

  .net-label {
    font-size: 0.9rem;
    color: var(--text-muted);
  }

  .net-value {
    font-size: 1.3rem;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    color: var(--text-muted);
  }

  .net-value.positive {
    color: var(--green);
  }
  .net-value.negative {
    color: var(--red);
  }

  .net-row.flash-net .net-value {
    animation: value-pop 0.6s cubic-bezier(0.34, 1.56, 0.64, 1);
  }

  .no-data-banner {
    text-align: center;
    padding: 2rem;
    color: var(--text-muted);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 0.75rem;
    margin-bottom: 1.5rem;
  }

  /* Platform breakdown */
  .platform-section {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 0.75rem;
    padding: 1rem 1.25rem;
    margin-bottom: 1rem;
  }

  .section-subtitle {
    font-size: 0.85rem;
    font-weight: 600;
    margin-bottom: 0.75rem;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .platform-bar-track {
    display: flex;
    height: 0.5rem;
    border-radius: 0.25rem;
    overflow: hidden;
    background: rgba(255, 255, 255, 0.05);
    margin-bottom: 0.6rem;
  }

  .platform-segment {
    height: 100%;
    transition: width 0.4s ease;
  }

  .seg-windows { background: #0078d4; }
  .seg-mac { background: #a3aaae; }
  .seg-linux { background: #e95420; }

  .platform-legend {
    display: flex;
    gap: 1.25rem;
    flex-wrap: wrap;
    font-size: 0.8rem;
    color: var(--text-muted);
  }

  .legend-item {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
  }

  .legend-dot {
    display: inline-block;
    width: 0.55rem;
    height: 0.55rem;
    border-radius: 50%;
  }

  .dot-windows { background: #0078d4; }
  .dot-mac { background: #a3aaae; }
  .dot-linux { background: #e95420; }

  /* Countries */
  .countries-section {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 0.75rem;
    padding: 1.5rem;
    margin-bottom: 1.5rem;
  }

  .countries-section h2 {
    font-size: 1.1rem;
    font-weight: 600;
    margin-bottom: 1rem;
  }

  .muted-count {
    font-size: 0.8rem;
    font-weight: 400;
    color: var(--text-muted);
  }

  .countries-table-wrap {
    overflow-x: auto;
  }

  .country-cell {
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .country-flag {
    font-size: 1.1em;
  }

  /* Platform columns in history */
  .platform-col {
    color: var(--text-muted);
    font-size: 0.7rem;
  }

  .platform-val {
    color: var(--text-muted);
    font-size: 0.8rem;
  }

  /* History table */
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
  .history-table td.muted {
    color: var(--text-muted);
    font-size: 0.8rem;
  }

  .history-table tbody tr.anomaly-row {
    background: rgba(239, 68, 68, 0.06);
    border-left: 3px solid var(--red);
  }

  .history-table tbody tr.anomaly-row:hover {
    background: rgba(239, 68, 68, 0.1);
  }

  .anomaly-badge {
    color: var(--red);
    font-size: 0.85rem;
    margin-right: 0.3rem;
    cursor: help;
  }

  .anomaly-detail {
    margin-top: 0.2rem;
  }

  .anomaly-detail-line {
    font-size: 0.75rem;
    color: var(--red);
    opacity: 0.85;
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
    .stats-row {
      grid-template-columns: repeat(2, 1fr);
    }

    .hero-title {
      font-size: 1.3rem;
    }

    .hero-overlay {
      padding: 1.5rem 1rem 1rem;
    }

    .stat-big-value {
      font-size: 1.3rem;
    }

    .stat-card {
      padding: 0.85rem 0.5rem;
    }

    .net-row {
      padding: 0.75rem 1rem;
    }

    .history-section {
      padding: 1rem;
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
