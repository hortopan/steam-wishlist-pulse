<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import type { TrackedGame } from './types';
  import { api, apiPost, AuthError } from './api';

  let {
    onLogout,
    showToast,
    hasSteamApiKey,
    onSwitchToSteam,
  }: {
    onLogout: () => void;
    showToast: (type: 'success' | 'error', text: string) => void;
    hasSteamApiKey: boolean;
    onSwitchToSteam: () => void;
  } = $props();

  let trackedGames = $state<TrackedGame[]>([]);
  let trackInput = $state('');
  let trackingLoading = $state(false);
  let trackingAction = $state(false);
  let pollTimer: ReturnType<typeof setInterval> | null = null;

  async function loadTrackedGames() {
    trackingLoading = true;
    try {
      trackedGames = await api<TrackedGame[]>('/admin/games');
    } catch (e: any) {
      if (e instanceof AuthError) { onLogout(); return; }
      showToast('error', e.message);
    } finally {
      trackingLoading = false;
    }
  }

  function startPolling() {
    stopPolling();
    pollTimer = setInterval(async () => {
      try {
        trackedGames = await api<TrackedGame[]>('/admin/games');
        // Stop polling if no games are syncing
        if (!trackedGames.some(g => g.is_syncing)) {
          stopPolling();
        }
      } catch {
        // Silently fail — next interval will retry
      }
    }, 5000);
  }

  function stopPolling() {
    if (pollTimer) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
  }

  // Start polling whenever any game is syncing
  $effect(() => {
    if (trackedGames.some(g => g.is_syncing) && !pollTimer) {
      startPolling();
    }
  });

  async function trackGame(e: Event) {
    e.preventDefault();
    if (!trackInput.trim()) return;
    trackingAction = true;
    try {
      const data = await apiPost<{ success: boolean; message?: string; error?: string }>('/admin/track', { input: trackInput.trim() });
      if (data.success) {
        showToast('success', data.message!);
        trackInput = '';
        await loadTrackedGames();
      } else {
        showToast('error', data.error || 'Failed to track game');
      }
    } catch (e: any) {
      if (e instanceof AuthError) { onLogout(); return; }
      showToast('error', e.message);
    } finally {
      trackingAction = false;
    }
  }

  async function untrackGame(appId: number, name: string) {
    if (!confirm(`Stop tracking "${name}"?`)) return;
    trackingAction = true;
    try {
      const data = await apiPost<{ success: boolean; message?: string; error?: string }>('/admin/untrack', { app_id: appId });
      if (data.success) {
        showToast('success', data.message!);
        await loadTrackedGames();
      } else {
        showToast('error', data.error || 'Failed to untrack game');
      }
    } catch (e: any) {
      if (e instanceof AuthError) { onLogout(); return; }
      showToast('error', e.message);
    } finally {
      trackingAction = false;
    }
  }

  async function syncGame(appId: number) {
    trackingAction = true;
    try {
      const data = await apiPost<{ success: boolean; message?: string; error?: string }>('/admin/sync', { app_id: appId });
      if (data.success) {
        showToast('success', data.message!);
        await loadTrackedGames();
      } else {
        showToast('error', data.error || 'Failed to start sync');
      }
    } catch (e: any) {
      if (e instanceof AuthError) { onLogout(); return; }
      showToast('error', e.message);
    } finally {
      trackingAction = false;
    }
  }

  function minutesAgo(isoDate: string | null): number {
    if (!isoDate) return 0;
    const diff = Date.now() - new Date(isoDate + 'Z').getTime();
    return Math.max(0, Math.round(diff / 60000));
  }

  function syncProgressPct(game: TrackedGame): number {
    if (game.sync_progress_total === 0) return 0;
    return Math.min(100, Math.round((game.sync_progress_crawled / game.sync_progress_total) * 100));
  }

  onMount(() => {
    loadTrackedGames();
  });

  onDestroy(() => {
    stopPolling();
  });
</script>

<section class="config-section">
  <h2>Track a Game</h2>
  {#if hasSteamApiKey}
    <p class="section-desc">
      Enter a Steam app ID or store URL to start tracking wishlists.
    </p>
    <form onsubmit={trackGame}>
      <div class="track-input-row">
        <input
          type="text"
          bind:value={trackInput}
          placeholder="App ID or Steam store URL"
          disabled={trackingAction}
        />
        <button
          type="submit"
          class="save-btn"
          disabled={trackingAction || !trackInput.trim()}
        >
          {trackingAction ? 'Adding...' : 'Track'}
        </button>
      </div>
      <span class="form-hint"
        >e.g. 4074510 or
        https://store.steampowered.com/app/4074510/Fleet_Hunters/</span
      >
    </form>
  {:else}
    <p class="section-desc">
      To track games, first configure your Steam API key in the <button class="link-btn" onclick={onSwitchToSteam}>Steam API</button> tab.
    </p>
  {/if}

  <h2 class="mt">Tracked Games</h2>
  {#if trackingLoading}
    <p class="section-desc">Loading...</p>
  {:else if trackedGames.length === 0}
    <p class="section-desc">No games are being tracked yet.</p>
  {:else}
    <div class="tracked-list">
      {#each trackedGames as game}
        <div class="tracked-game">
          {#if game.image_url}
            <img
              src={game.image_url}
              alt={game.name}
              class="game-thumb"
            />
          {/if}
          <div class="game-info">
            <span class="game-name">{game.name}</span>
            <span class="game-id">ID: {game.app_id}</span>
            {#if game.is_syncing}
              <div class="sync-status">
                <span class="sync-label">Syncing {game.sync_progress_crawled}/{game.sync_progress_total} days</span>
                <div class="sync-progress-bar">
                  <div class="sync-progress-fill" style="width: {syncProgressPct(game)}%"></div>
                </div>
              </div>
            {/if}
          </div>
          <div class="game-actions">
            <button
              class="sync-btn"
              onclick={() => syncGame(game.app_id)}
              disabled={trackingAction || game.is_syncing || game.cooldown_active}
              title={game.is_syncing ? 'Sync in progress' : game.cooldown_active ? 'Recently synced — please wait' : 'Re-sync all historical data'}
            >
              {#if game.is_syncing}
                Syncing...
              {:else if game.cooldown_active}
                Synced {minutesAgo(game.last_sync_completed_at)}m ago
              {:else}
                Full Sync
              {/if}
            </button>
            <button
              class="untrack-btn"
              onclick={() => untrackGame(game.app_id, game.name)}
              disabled={trackingAction}
            >
              Untrack
            </button>
          </div>
        </div>
      {/each}
    </div>
  {/if}
</section>

<style>
  .config-section {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 0.75rem;
    padding: 1.5rem;
  }

  @media (max-width: 600px) {
    .config-section {
      padding: 1rem;
    }
  }

  .config-section h2 {
    font-size: 1.2rem;
    font-weight: 600;
    margin-bottom: 0.25rem;
  }

  .mt {
    margin-top: 1.5rem;
  }

  .section-desc {
    color: var(--text-muted);
    font-size: 0.85rem;
    margin-bottom: 1.25rem;
  }

  .link-btn {
    background: none;
    border: none;
    color: var(--accent);
    cursor: pointer;
    padding: 0;
    font: inherit;
    text-decoration: underline;
  }

  .link-btn:hover {
    opacity: 0.8;
  }

  .form-hint {
    display: block;
    font-size: 0.75rem;
    color: var(--text-muted);
    margin-top: 0.25rem;
    word-break: break-all;
  }

  .save-btn {
    padding: 0.75rem 1.5rem;
    background: var(--accent);
    color: white;
    border: none;
    border-radius: 0.5rem;
    font-size: 0.95rem;
    font-weight: 600;
    cursor: pointer;
    transition: opacity 0.2s;
    margin-top: 0.5rem;
  }

  .save-btn:hover:not(:disabled) {
    opacity: 0.9;
  }

  .save-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .track-input-row {
    display: flex;
    gap: 0.5rem;
    margin-bottom: 0.25rem;
  }

  .track-input-row input {
    flex: 1;
    padding: 0.75rem 1rem;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 0.5rem;
    color: var(--text);
    font-size: 0.95rem;
    outline: none;
    transition: border-color 0.2s;
  }

  .track-input-row input:focus {
    border-color: var(--accent);
  }

  .track-input-row .save-btn {
    margin-top: 0;
    white-space: nowrap;
  }

  .tracked-list {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    margin-top: 0.75rem;
  }

  .tracked-game {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.65rem 0.75rem;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 0.5rem;
  }

  .game-thumb {
    width: 120px;
    height: 45px;
    object-fit: cover;
    border-radius: 0.25rem;
    flex-shrink: 0;
  }

  .game-info {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    min-width: 0;
  }

  .game-name {
    font-weight: 600;
    font-size: 0.9rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .game-id {
    font-size: 0.75rem;
    color: var(--text-muted);
  }

  .game-actions {
    display: flex;
    gap: 0.4rem;
    flex-shrink: 0;
  }

  .sync-status {
    margin-top: 0.25rem;
  }

  .sync-label {
    font-size: 0.7rem;
    color: var(--accent);
    font-weight: 500;
  }

  .sync-progress-bar {
    width: 100%;
    max-width: 180px;
    height: 4px;
    background: var(--border);
    border-radius: 2px;
    margin-top: 0.2rem;
    overflow: hidden;
  }

  .sync-progress-fill {
    height: 100%;
    background: var(--accent);
    border-radius: 2px;
    transition: width 0.5s ease;
  }

  .sync-btn {
    background: none;
    border: 1px solid var(--border);
    color: var(--accent);
    padding: 0.35rem 0.75rem;
    border-radius: 0.375rem;
    font-size: 0.8rem;
    cursor: pointer;
    transition:
      border-color 0.2s,
      color 0.2s,
      opacity 0.2s;
    white-space: nowrap;
  }

  .sync-btn:hover:not(:disabled) {
    border-color: var(--accent);
  }

  .sync-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .untrack-btn {
    background: none;
    border: 1px solid var(--border);
    color: var(--text-muted);
    padding: 0.35rem 0.75rem;
    border-radius: 0.375rem;
    font-size: 0.8rem;
    cursor: pointer;
    transition:
      border-color 0.2s,
      color 0.2s;
    flex-shrink: 0;
  }

  @media (max-width: 600px) {
    .tracked-game {
      flex-wrap: wrap;
    }

    .game-thumb {
      width: 80px;
      height: 30px;
    }

    .game-info {
      flex: 1 1 calc(100% - 100px);
    }

    .game-actions {
      margin-left: auto;
    }
  }

  .untrack-btn:hover:not(:disabled) {
    border-color: var(--red);
    color: var(--red);
  }

  .untrack-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
</style>
