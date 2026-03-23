<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { playNotificationSound } from "./notificationSound";
  import { api, AuthError } from "./api";
  import { timeAgo, formatDate } from "./utils";
  import { POLL_INTERVAL, TICK_INTERVAL, FLASH_DURATION } from "./constants";
  import type { GameReport } from "./types";

  interface ApiResponse {
    games: GameReport[];
    error?: string;
  }

  let {
    accessLevel,
    onLogout,
    onNavigateAdmin,
    onNavigateGame,
  }: {
    accessLevel: string;
    onLogout: () => void;
    onNavigateAdmin: () => void;
    onNavigateGame: (appId: number) => void;
  } = $props();

  let games = $state<GameReport[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let now = $state(Date.now());
  let pollTimer: ReturnType<typeof setTimeout> | null = null;
  let tickTimer: ReturnType<typeof setInterval> | null = null;

  // Animation: track which cards/stats changed on last poll
  let flashCards = $state<Set<number>>(new Set());
  let prevGamesMap: Map<number, GameReport> = new Map();

  function schedulePoll() {
    pollTimer = setTimeout(async () => {
      await fetchData();
      schedulePoll();
    }, POLL_INTERVAL);
  }

  async function fetchData() {
    if (games.length === 0) loading = true;
    error = null;
    try {
      const data = await api<ApiResponse>("/wishlist");
      if (data.error) throw new Error(data.error);

      // Detect changed games for flash animation
      if (prevGamesMap.size > 0) {
        const changed = new Set<number>();
        for (const game of data.games) {
          const prev = prevGamesMap.get(game.app_id);
          if (
            prev &&
            (prev.adds !== game.adds ||
              prev.deletes !== game.deletes ||
              prev.purchases !== game.purchases ||
              prev.gifts !== game.gifts)
          ) {
            changed.add(game.app_id);
          }
        }
        if (changed.size > 0) {
          flashCards = changed;
          playNotificationSound();
          setTimeout(() => (flashCards = new Set()), FLASH_DURATION);
        }
      }

      prevGamesMap = new Map(data.games.map((g) => [g.app_id, { ...g }]));
      games = data.games;
    } catch (e: any) {
      if (e instanceof AuthError) { onLogout(); return; }
      error = e.message;
    } finally {
      loading = false;
    }
  }

  function handleCardKeydown(e: KeyboardEvent, appId: number) {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      onNavigateGame(appId);
    }
  }

  onMount(() => {
    fetchData();
    schedulePoll();
    tickTimer = setInterval(() => {
      now = Date.now();
    }, TICK_INTERVAL);
  });

  onDestroy(() => {
    if (pollTimer) clearTimeout(pollTimer);
    if (tickTimer) clearInterval(tickTimer);
  });
</script>

{#if error}
  <div class="error-banner">
    <span>Error: {error}</span>
    <button onclick={fetchData}>Retry</button>
  </div>
{/if}

{#if loading && games.length === 0}
  <div class="loading">
    <div class="spinner"></div>
  </div>
{:else if games.length === 0}
  <div class="empty">
    No tracked games yet.
    {#if accessLevel === "admin"}
      Configure your Steam API key in the <button
        class="link-btn"
        onclick={onNavigateAdmin}>Admin panel</button
      > to get started.
    {:else}
      Ask an admin to add games via the admin panel or Telegram bot.
    {/if}
  </div>
{:else}
  <div class="grid">
    {#each games as game}
      <div
        class="card clickable"
        class:flash={flashCards.has(game.app_id)}
        role="button"
        tabindex="0"
        onclick={() => onNavigateGame(game.app_id)}
        onkeydown={(e) => handleCardKeydown(e, game.app_id)}
      >
        {#if game.image_url}
          <img class="card-image" src={game.image_url} alt={game.name} />
        {/if}
        <div class="card-body">
          <div class="card-header">
            <h2 class="game-name">{game.name}</h2>
            <span class="app-id">#{game.app_id}</span>
          </div>
          <div class="card-date">
            {#if game.date}
              {formatDate(game.date)}
              {#if game.changed_at}
                <span
                  class="changed-at"
                  title={new Date(game.changed_at).toLocaleString()}
                  >· last updated {timeAgo(game.changed_at, now)}</span
                >
              {/if}
            {:else}
              <span class="no-data">No data yet</span>
            {/if}
          </div>
          <div
            class="stat-net"
            class:positive={game.adds - game.deletes >= 0}
            class:negative={game.adds - game.deletes < 0}
          >
            <span class="net-value"
              >{game.adds - game.deletes >= 0 ? "+" : ""}{(
                game.adds - game.deletes
              ).toLocaleString()}</span
            >
            <span class="net-label">Net Wishlists</span>
          </div>
          <div class="stats">
            <div class="stat stat-adds">
              <span class="stat-value">{game.adds.toLocaleString()}</span>
              <span class="stat-label">Adds</span>
            </div>
            <div class="stat stat-deletes">
              <span class="stat-value">{game.deletes.toLocaleString()}</span>
              <span class="stat-label">Deletes</span>
            </div>
            <div class="stat stat-purchases">
              <span class="stat-value">{game.purchases.toLocaleString()}</span>
              <span class="stat-label">Purchases</span>
            </div>
            <div class="stat stat-gifts">
              <span class="stat-value">{game.gifts.toLocaleString()}</span>
              <span class="stat-label">Gifts</span>
            </div>
          </div>
        </div>
      </div>
    {/each}
  </div>
{/if}

<style>
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

  .empty {
    text-align: center;
    padding: 4rem 2rem;
    color: var(--text-muted);
    font-size: 1.1rem;
  }

  .link-btn {
    background: none;
    border: none;
    color: var(--accent);
    cursor: pointer;
    font-size: inherit;
    text-decoration: underline;
    padding: 0;
  }

  .grid {
    display: flex;
    flex-wrap: wrap;
    gap: 1rem;
    justify-content: center;
  }

  .card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 0.75rem;
    overflow: hidden;
    transition:
      border-color 0.2s,
      transform 0.2s;
    width: 100%;
    max-width: 400px;
    min-width: 280px;
    flex: 1 1 280px;
  }

  @media (max-width: 600px) {
    .card {
      min-width: 0;
      flex: 1 1 100%;
    }
  }

  .card.flash {
    animation: pulse-card 1.2s ease-out;
  }

  @keyframes pulse-card {
    0% {
      border-color: var(--accent);
      box-shadow: 0 0 16px rgba(99, 102, 241, 0.3);
      transform: scale(1.02);
    }
    100% {
      border-color: var(--border);
      box-shadow: none;
      transform: scale(1);
    }
  }

  .card-image {
    width: 100%;
    height: auto;
    display: block;
  }

  .card-body {
    padding: 1.25rem;
  }

  .card.clickable {
    cursor: pointer;
    text-align: left;
  }

  .card.clickable:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .card:hover {
    border-color: var(--accent);
  }

  .card-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    margin-bottom: 0.25rem;
  }

  .game-name {
    font-size: 1.1rem;
    font-weight: 600;
    line-height: 1.3;
  }

  .app-id {
    color: var(--text-muted);
    font-size: 0.8rem;
    white-space: nowrap;
    margin-left: 0.5rem;
  }

  .card-date {
    color: var(--text-muted);
    font-size: 0.85rem;
    margin-bottom: 1rem;
  }

  .changed-at {
    color: var(--text-muted);
    opacity: 0.7;
  }

  .stats {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 0.5rem;
  }

  .stat {
    text-align: center;
    padding: 0.5rem 0.25rem;
    border-radius: 0.5rem;
    background: rgba(255, 255, 255, 0.03);
  }

  .stat-value {
    display: block;
    font-size: 1.1rem;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }

  .stat-label {
    display: block;
    font-size: 0.7rem;
    color: var(--text-muted);
    margin-top: 0.2rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .stat-adds .stat-value {
    color: var(--green);
  }
  .stat-deletes .stat-value {
    color: var(--red);
  }
  .stat-purchases .stat-value {
    color: var(--blue);
  }
  .stat-gifts .stat-value {
    color: var(--amber);
  }

  .stat-net {
    text-align: center;
    padding: 0.5rem;
    margin-bottom: 0.75rem;
    border-radius: 0.5rem;
    background: rgba(255, 255, 255, 0.03);
  }

  .net-value {
    font-size: 1.4rem;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }

  .net-label {
    display: block;
    font-size: 0.7rem;
    color: var(--text-muted);
    margin-top: 0.15rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .stat-net.positive .net-value {
    color: var(--green);
  }
  .stat-net.negative .net-value {
    color: var(--red);
  }
</style>
