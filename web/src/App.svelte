<script lang="ts">
  import { onMount } from 'svelte';
  import { initAudio, isSoundEnabled, setSoundEnabled } from './notificationSound';
  import { startPolling, stopPolling, subscribe, hasAnyIssue } from './healthStore';
  import LogoBrand from './LogoBrand.svelte';
  import Login from './Login.svelte';
  import Setup from './Setup.svelte';
  import Dashboard from './Dashboard.svelte';
  import Admin from './Admin.svelte';
  import GameDetail from './GameDetail.svelte';

  type View = 'loading' | 'setup' | 'login' | 'dashboard' | 'admin' | 'game';

  let view = $state<View>('loading');
  let accessLevel = $state<string | null>(null);
  let selectedGameId = $state<number | null>(null);
  let soundEnabled = $state(isSoundEnabled());
  let hasHealthIssue = $state(false);
  let appVersion = $state<string | null>(null);
  let latestVersion = $state<string | null>(null);
  let unsubHealth: (() => void) | null = null;

  function toggleSound() {
    soundEnabled = !soundEnabled;
    setSoundEnabled(soundEnabled);
  }

  const pageTitles: Record<View, string> = {
    loading: 'Wishlist Pulse',
    setup: 'Setup — Wishlist Pulse',
    login: 'Sign In — Wishlist Pulse',
    dashboard: 'Dashboard — Wishlist Pulse',
    admin: 'Admin — Wishlist Pulse',
    game: 'Game — Wishlist Pulse',
  };

  $effect(() => {
    document.title = pageTitles[view];
  });

  async function checkAuth() {
    try {
      const res = await fetch('/api/auth/status');
      const data = await res.json();

      if (data.version) appVersion = data.version;
      if (data.latest_version) latestVersion = data.latest_version;
      if (data.setup_required) {
        view = 'setup';
      } else if (data.authenticated) {
        accessLevel = data.access_level;
        if (data.access_level === 'admin') {
          startPolling();
          unsubHealth = subscribe(() => { hasHealthIssue = hasAnyIssue(); });
        }
        const path = window.location.pathname;
        const gameMatch = path.match(/^\/game\/(\d+)/);
        if (path === '/admin' && data.access_level === 'admin') {
          view = 'admin';
        } else if (gameMatch) {
          selectedGameId = parseInt(gameMatch[1], 10);
          view = 'game';
        } else {
          view = 'dashboard';
        }
      } else {
        view = 'login';
      }
    } catch {
      view = 'login';
    }
  }

  function onLogin() {
    checkAuth();
  }

  function onSetup() {
    checkAuth();
  }

  function onLogout() {
    accessLevel = null;
    hasHealthIssue = false;
    if (unsubHealth) { unsubHealth(); unsubHealth = null; }
    stopPolling();
    view = 'login';
    window.history.pushState({}, '', '/');
  }

  async function handleLogout() {
    await fetch('/api/auth/logout', { method: 'POST' });
    onLogout();
  }

  function onNavigateAdmin() {
    view = 'admin';
    window.history.pushState({}, '', '/admin');
  }

  function onNavigateGame(appId: number) {
    selectedGameId = appId;
    view = 'game';
    window.history.pushState({}, '', `/game/${appId}`);
  }

  function onBackToDashboard() {
    view = 'dashboard';
    selectedGameId = null;
    window.history.pushState({}, '', '/');
  }

  onMount(() => {
    initAudio();
    checkAuth();

    window.addEventListener('popstate', () => {
      if (accessLevel) {
        const path = window.location.pathname;
        const gameMatch = path.match(/^\/game\/(\d+)/);
        if (path === '/admin' && accessLevel === 'admin') {
          view = 'admin';
        } else if (gameMatch) {
          selectedGameId = parseInt(gameMatch[1], 10);
          view = 'game';
        } else {
          selectedGameId = null;
          view = 'dashboard';
        }
      }
    });
  });
</script>

{#if view === 'dashboard' || view === 'admin' || view === 'game'}
  <header class="app-header">
    <button class="logo-link" onclick={onBackToDashboard}>
      <LogoBrand />
    </button>
    <div class="header-actions">
      <button onclick={toggleSound} class="header-btn sound-btn" title={soundEnabled ? 'Mute notifications' : 'Unmute notifications'}>
        {#if soundEnabled}
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />
            <path d="M19.07 4.93a10 10 0 0 1 0 14.14" />
            <path d="M15.54 8.46a5 5 0 0 1 0 7.07" />
          </svg>
        {:else}
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />
            <line x1="23" y1="9" x2="17" y2="15" />
            <line x1="17" y1="9" x2="23" y2="15" />
          </svg>
        {/if}
      </button>
      {#if (view === 'dashboard' || view === 'game') && accessLevel === 'admin'}
        <button onclick={onNavigateAdmin} class="header-btn admin-btn">Admin{#if hasHealthIssue}<span class="health-dot"></span>{/if}</button>
      {/if}
      {#if view === 'dashboard' || view === 'admin' || view === 'game'}
        <button onclick={handleLogout} class="header-btn logout-btn">Logout</button>
      {/if}
    </div>
  </header>
{/if}

{#if view === 'loading'}
  <div class="loading-screen">
    <LogoBrand size="large" />
    <div class="spinner"></div>
  </div>
{:else if view === 'setup'}
  <Setup {onSetup} />
{:else if view === 'login'}
  <Login {onLogin} />
{:else if view === 'dashboard'}
  <Dashboard accessLevel={accessLevel || 'read_only'} {onLogout} {onNavigateAdmin} {onNavigateGame} />
{:else if view === 'game' && selectedGameId}
  <GameDetail appId={selectedGameId} onBack={onBackToDashboard} {onLogout} />
{:else if view === 'admin'}
  <Admin {onLogout} />
{/if}

{#if appVersion}
  <footer class="app-footer">
    <a href="https://github.com/hortopan/steam-wishlist-pulse" target="_blank" rel="noopener noreferrer">Wishlist Pulse</a> v{appVersion}
    {#if latestVersion}
      <a href="https://github.com/hortopan/steam-wishlist-pulse/releases/latest" target="_blank" rel="noopener noreferrer" class="update-badge">Update available: v{latestVersion}</a>
    {/if}
  </footer>
{/if}

<style>
  .app-footer {
    text-align: center;
    margin-top: 3rem;
    padding: 1rem 0;
    border-top: 1px solid var(--border);
    color: var(--text-muted);
    font-size: 0.75rem;
    opacity: 0.5;
  }

  .app-footer a {
    color: var(--text-muted);
    text-decoration: none;
  }

  .app-footer a:hover {
    color: var(--accent);
  }

  .update-badge {
    display: inline-block;
    margin-left: 0.5rem;
    padding: 0.15rem 0.5rem;
    font-size: 0.7rem;
    background: var(--accent);
    color: var(--bg, #1a1a2e) !important;
    border-radius: 999px;
    text-decoration: none !important;
    opacity: 1;
    font-weight: 600;
    transition: opacity 0.2s;
  }

  .update-badge:hover {
    opacity: 0.85;
  }
  .app-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding-bottom: 1rem;
    margin-bottom: 1.5rem;
    border-bottom: 1px solid var(--border);
    gap: 0.5rem;
    flex-wrap: wrap;
  }

  .logo-link {
    cursor: pointer;
    display: flex;
    align-items: center;
    gap: 0.75rem;
    background: none;
    border: none;
    padding: 0;
    color: inherit;
    font: inherit;
  }

  .header-actions {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }

  .header-btn {
    background: var(--surface);
    color: var(--text-muted);
    border: 1px solid var(--border);
    padding: 0.5rem 1rem;
    border-radius: 0.5rem;
    cursor: pointer;
    font-size: 0.85rem;
    transition: border-color 0.2s, color 0.2s;
    white-space: nowrap;
  }

  @media (max-width: 600px) {
    .header-btn {
      padding: 0.4rem 0.65rem;
      font-size: 0.8rem;
    }
  }

  .header-btn:hover {
    border-color: var(--accent);
    color: var(--text);
  }

  .sound-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0.5rem;
  }

  .admin-btn {
    border-color: var(--accent);
    color: var(--accent);
    position: relative;
  }

  .health-dot {
    display: inline-block;
    width: 0.5rem;
    height: 0.5rem;
    background: var(--red, #ef4444);
    border-radius: 50%;
    margin-left: 0.35rem;
    vertical-align: middle;
  }

  .logout-btn {
    color: var(--text-muted);
  }

  .loading-screen {
    display: flex;
    flex-direction: column;
    justify-content: center;
    align-items: center;
    min-height: 80vh;
    gap: 1.5rem;
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
    to { transform: rotate(360deg); }
  }
</style>
