<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { subscribe, fetchHealth, type HealthData } from "./healthStore";
  import { api, apiPost, AuthError } from "./api";
  import { TOAST_DURATION, TOAST_DISMISS_DURATION } from "./constants";
  import AdminGamesTab from "./AdminGamesTab.svelte";
  import AdminPasswordsTab from "./AdminPasswordsTab.svelte";

  let { onLogout }: { onLogout: () => void } = $props();

  let hasSteamApiKey = $state(false);
  let hasTelegramBotToken = $state(false);
  let hasDiscordBotToken = $state(false);
  let steamApiKey = $state("");
  let telegramBotToken = $state("");
  let telegramAdminIds = $state("");
  let telegramEnabled = $state(false);
  let discordBotToken = $state("");
  let discordAdminIds = $state("");
  let discordEnabled = $state(false);
  let trackingRetentionDays = $state(90);

  let activeTab = $state<"games" | "steam" | "telegram" | "discord" | "passwords">("games");

  let loading = $state(true);
  let saving = $state(false);

  let health = $state<HealthData | null>(null);
  let unsubHealth: (() => void) | null = null;

  // Toast system
  interface Toast {
    id: number;
    type: "success" | "error";
    text: string;
    dismissing: boolean;
  }
  let toasts = $state<Toast[]>([]);
  let toastId = 0;

  function showToast(type: "success" | "error", text: string) {
    const id = ++toastId;
    toasts.push({ id, type, text, dismissing: false });
    setTimeout(() => {
      const t = toasts.find((t) => t.id === id);
      if (t) t.dismissing = true;
      setTimeout(() => {
        toasts = toasts.filter((t) => t.id !== id);
      }, TOAST_DISMISS_DURATION);
    }, TOAST_DURATION);
  }

  async function loadConfig() {
    loading = true;
    try {
      const data = await api<any>("/admin/config");
      hasSteamApiKey = data.has_steam_api_key || false;
      hasTelegramBotToken = data.has_telegram_bot_token || false;
      hasDiscordBotToken = data.has_discord_bot_token || false;
      steamApiKey = "";
      telegramBotToken = "";
      telegramAdminIds = data.telegram_admin_ids || "";
      telegramEnabled = data.telegram_enabled || false;
      discordBotToken = "";
      discordAdminIds = data.discord_admin_ids || "";
      discordEnabled = data.discord_enabled || false;
      trackingRetentionDays = data.tracking_retention_days || 90;
    } catch (e: any) {
      if (e instanceof AuthError) { onLogout(); return; }
      showToast("error", e.message);
    } finally {
      loading = false;
    }
  }

  async function saveConfig(e: Event) {
    e.preventDefault();
    saving = true;
    try {
      const body: Record<string, any> = {
        telegram_admin_ids: telegramAdminIds,
        telegram_enabled: telegramEnabled,
        discord_admin_ids: discordAdminIds,
        discord_enabled: discordEnabled,
        tracking_retention_days: trackingRetentionDays,
      };
      if (steamApiKey) body.steam_api_key = steamApiKey;
      if (telegramBotToken) body.telegram_bot_token = telegramBotToken;
      if (discordBotToken) body.discord_bot_token = discordBotToken;

      const data = await apiPost<{ success: boolean; error?: string }>("/admin/config", body);
      if (data.success) {
        showToast("success", "Configuration saved and applied.");
        if (steamApiKey) hasSteamApiKey = true;
        if (telegramBotToken) hasTelegramBotToken = true;
        if (discordBotToken) hasDiscordBotToken = true;
        steamApiKey = "";
        telegramBotToken = "";
        discordBotToken = "";
        setTimeout(fetchHealth, 500);
      } else {
        showToast("error", data.error || "Failed to save");
      }
    } catch (e: any) {
      if (e instanceof AuthError) { onLogout(); return; }
      showToast("error", e.message);
    } finally {
      saving = false;
    }
  }

  onMount(() => {
    loadConfig();
    unsubHealth = subscribe((data) => { health = data; });
  });

  onDestroy(() => {
    if (unsubHealth) unsubHealth();
  });
</script>

<h1 class="page-title">Admin Panel</h1>

{#if loading}
  <div class="loading">Loading configuration...</div>
{:else}
  <div class="tabs">
    <button
      class="tab"
      class:active={activeTab === "games"}
      onclick={() => (activeTab = "games")}>Games</button
    >
    <button
      class="tab"
      class:active={activeTab === "steam"}
      onclick={() => (activeTab = "steam")}>Steam{#if health}<span class="tab-status {health.steam.status === 'ok' ? 'status-ok' : health.steam.status === 'disabled' ? '' : 'status-error'}">{health.steam.status === 'ok' ? '\u2713' : health.steam.status === 'disabled' ? '' : '\u26A0'}</span>{/if}</button
    >
    <button
      class="tab"
      class:active={activeTab === "telegram"}
      onclick={() => (activeTab = "telegram")}>Telegram{#if health && health.telegram.status !== 'disabled'}<span class="tab-status {health.telegram.status === 'ok' ? 'status-ok' : 'status-error'}">{health.telegram.status === 'ok' ? '\u2713' : '\u26A0'}</span>{/if}</button
    >
    <button
      class="tab"
      class:active={activeTab === "discord"}
      onclick={() => (activeTab = "discord")}>Discord{#if health && health.discord.status !== 'disabled'}<span class="tab-status {health.discord.status === 'ok' ? 'status-ok' : 'status-error'}">{health.discord.status === 'ok' ? '\u2713' : '\u26A0'}</span>{/if}</button
    >
    <button
      class="tab"
      class:active={activeTab === "passwords"}
      onclick={() => (activeTab = "passwords")}>Access &amp; Passwords</button
    >
  </div>

  <div class="tab-content">
    {#if activeTab === "games"}
      <AdminGamesTab {onLogout} {showToast} {hasSteamApiKey} onSwitchToSteam={() => (activeTab = "steam")} />
    {:else if activeTab === "steam"}
      <section class="config-section">
        {#if health && health.steam.status !== 'ok' && health.steam.status !== 'disabled' && health.steam.message}
          <div class="health-alert">{health.steam.message}</div>
        {/if}
        <h2>Steam API</h2>
        <p class="section-desc">
          Configure your Steam Web API key for wishlist data access.
        </p>
        <form onsubmit={saveConfig}>
          <div class="form-group">
            <label for="steam-key">Steam API Key</label>
            {#if hasSteamApiKey}
              <span class="secret-status configured">Configured</span>
            {:else}
              <span class="secret-status not-configured">Not configured</span>
            {/if}
            <input
              id="steam-key"
              type="password"
              bind:value={steamApiKey}
              placeholder={hasSteamApiKey
                ? "Enter new key to replace"
                : "Enter Steam API key"}
              disabled={saving}
            />
            <div class="api-key-help">
              <p>A <strong>Financial API Group</strong> web API key is required. To set one up:</p>
              <ol>
                <li>Go to <a href="https://partner.steamgames.com" target="_blank" rel="noopener">Steamworks</a> and navigate to <strong>Users &amp; Permissions &rarr; Manage Groups</strong>.</li>
                <li>Create a new <strong>Financial API Group</strong> (or use an existing one).</li>
                <li>Ensure the group has both <strong>General API Methods</strong> and <strong>Financial API Methods</strong> access enabled.</li>
                <li>Go to <strong>Financial API &rarr; Manage Web API Key</strong> and copy the key.</li>
              </ol>
              <p class="api-key-why">This key is used to retrieve wishlist data via the <code>IPartnerFinancialsService/GetAppWishlistReporting</code> partner API endpoint.</p>
            </div>
          </div>

          <h2 class="mt">Data Retention</h2>
          <p class="section-desc">
            How long to keep tracking snapshots before automatic cleanup.
          </p>

          <div class="form-group">
            <label for="retention-days">Retention Period (days)</label>
            <input
              id="retention-days"
              type="number"
              min="1"
              bind:value={trackingRetentionDays}
              disabled={saving}
            />
            <span class="form-hint"
              >Snapshots older than this will be automatically purged</span
            >
          </div>

          <button type="submit" class="save-btn" disabled={saving}>
            {saving ? "Saving..." : "Save Configuration"}
          </button>
        </form>
      </section>
    {:else if activeTab === "telegram"}
      <section class="config-section">
        {#if health && health.telegram.status === 'error' && health.telegram.message}
          <div class="health-alert">{health.telegram.message}</div>
        {/if}
        <h2>Telegram Bot</h2>
        <p class="section-desc">
          Configure the Telegram bot for command-based management.
        </p>
        <form onsubmit={saveConfig}>
          <div class="form-group checkbox-group">
            <label>
              <input
                type="checkbox"
                bind:checked={telegramEnabled}
                disabled={saving}
              />
              <span>Enable Telegram bot</span>
            </label>
          </div>

          {#if telegramEnabled}
            <div class="form-group">
              <label for="tg-token">Bot Token</label>
              {#if hasTelegramBotToken}
                <span class="secret-status configured">Configured</span>
              {:else}
                <span class="secret-status not-configured">Not configured</span>
              {/if}
              <input
                id="tg-token"
                type="password"
                bind:value={telegramBotToken}
                placeholder={hasTelegramBotToken
                  ? "Enter new token to replace"
                  : "Token from @BotFather"}
                disabled={saving}
              />
            </div>

            <div class="form-group">
              <label for="tg-ids">Admin User IDs</label>
              <input
                id="tg-ids"
                type="text"
                bind:value={telegramAdminIds}
                placeholder="comma-separated user IDs (e.g. 12345678,87654321)"
                disabled={saving}
              />
              <span class="form-hint"
                >Comma-separated numeric Telegram user IDs. Send /whoami to the
                bot to find yours.</span
              >
            </div>
          {/if}

          <button type="submit" class="save-btn" disabled={saving}>
            {saving ? "Saving..." : "Save Configuration"}
          </button>
        </form>
      </section>
    {:else if activeTab === "discord"}
      <section class="config-section">
        {#if health && health.discord.status === 'error' && health.discord.message}
          <div class="health-alert">{health.discord.message}</div>
        {/if}
        <h2>Discord Bot</h2>
        <p class="section-desc">
          Configure the Discord bot for slash command management and notifications.
        </p>
        <form onsubmit={saveConfig}>
          <div class="form-group checkbox-group">
            <label>
              <input
                type="checkbox"
                bind:checked={discordEnabled}
                disabled={saving}
              />
              <span>Enable Discord bot</span>
            </label>
          </div>

          {#if discordEnabled}
            <div class="form-group">
              <label for="dc-token">Bot Token</label>
              {#if hasDiscordBotToken}
                <span class="secret-status configured">Configured</span>
              {:else}
                <span class="secret-status not-configured">Not configured</span>
              {/if}
              <input
                id="dc-token"
                type="password"
                bind:value={discordBotToken}
                placeholder={hasDiscordBotToken
                  ? "Enter new token to replace"
                  : "Token from Discord Developer Portal"}
                disabled={saving}
              />
              <div class="api-key-help">
                <p>A Discord bot token is required. To set one up:</p>
                <ol>
                  <li>Go to the <a href="https://discord.com/developers/applications" target="_blank" rel="noopener">Discord Developer Portal</a> and create a new application.</li>
                  <li>Navigate to <strong>Bot</strong> and click <strong>Reset Token</strong> to generate a bot token.</li>
                  <li>Under <strong>OAuth2 &rarr; URL Generator</strong>, select the <strong>bot</strong> and <strong>applications.commands</strong> scopes.</li>
                  <li>Use the generated URL to invite the bot to your server.</li>
                </ol>
              </div>
            </div>

            <div class="form-group">
              <label for="dc-ids">Admin User IDs</label>
              <input
                id="dc-ids"
                type="text"
                bind:value={discordAdminIds}
                placeholder="comma-separated user IDs (e.g. 123456789012345678)"
                disabled={saving}
              />
              <span class="form-hint"
                >Comma-separated numeric Discord user IDs. Right-click your
                username with Developer Mode enabled to copy yours.</span
              >
            </div>
          {/if}

          <button type="submit" class="save-btn" disabled={saving}>
            {saving ? "Saving..." : "Save Configuration"}
          </button>
        </form>
      </section>
    {:else if activeTab === "passwords"}
      <AdminPasswordsTab {onLogout} {showToast} />
    {/if}
  </div>
{/if}

{#if toasts.length > 0}
  <div class="toast-container">
    {#each toasts as toast (toast.id)}
      <div class="toast toast-{toast.type}" class:toast-dismissing={toast.dismissing}>
        {toast.text}
      </div>
    {/each}
  </div>
{/if}

<style>
  .api-key-help {
    margin-top: 0.75rem;
    padding: 0.75rem 1rem;
    background: var(--bg-card, #23272e);
    border: 1px solid var(--border, #333);
    border-radius: 6px;
    font-size: 0.85rem;
    line-height: 1.5;
    color: var(--text-muted, #aaa);
  }
  .api-key-help p {
    margin: 0 0 0.5rem;
  }
  .api-key-help ol {
    margin: 0 0 0.5rem;
    padding-left: 1.25rem;
  }
  .api-key-help li {
    margin-bottom: 0.25rem;
  }
  .api-key-help a {
    color: var(--accent);
  }
  .api-key-help code {
    font-size: 0.8rem;
    background: var(--bg, #1a1d23);
    padding: 0.1rem 0.35rem;
    border-radius: 3px;
  }
  .api-key-why {
    margin-bottom: 0 !important;
    opacity: 0.8;
  }

  .page-title {
    font-size: 1.3rem;
    font-weight: 600;
    margin-bottom: 1.5rem;
    color: var(--text);
  }

  .loading {
    text-align: center;
    padding: 4rem 2rem;
    color: var(--text-muted);
    font-size: 1.1rem;
  }

  .tabs {
    display: flex;
    gap: 0.25rem;
    margin-bottom: 1.5rem;
    border-bottom: 1px solid var(--border);
    padding-bottom: 0;
  }

  .tab {
    padding: 0.65rem 1.25rem;
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    color: var(--text-muted);
    font-size: 0.9rem;
    font-weight: 500;
    cursor: pointer;
    transition:
      color 0.2s,
      border-color 0.2s;
    margin-bottom: -1px;
  }

  .tab:hover {
    color: var(--text);
  }

  .tab.active {
    color: var(--accent);
    border-bottom-color: var(--accent);
  }

  .tab-status {
    margin-left: 0.4rem;
    font-size: 0.75rem;
  }

  .tab-status.status-ok {
    color: var(--green);
  }

  .tab-status.status-error {
    color: var(--red);
  }

  .health-alert {
    background: rgba(239, 68, 68, 0.1);
    border: 1px solid var(--red);
    color: var(--red);
    padding: 0.65rem 1rem;
    border-radius: 0.5rem;
    font-size: 0.85rem;
    font-weight: 500;
    margin-bottom: 1rem;
  }

  .config-section {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 0.75rem;
    padding: 1.5rem;
  }

  .secret-status {
    display: inline-block;
    font-size: 0.75rem;
    font-weight: 600;
    padding: 0.15rem 0.5rem;
    border-radius: 0.25rem;
    margin-bottom: 0.5rem;
  }

  .secret-status.configured {
    background: rgba(34, 197, 94, 0.1);
    color: var(--green);
  }

  .secret-status.not-configured {
    background: rgba(239, 68, 68, 0.1);
    color: var(--red);
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

  .form-group {
    margin-bottom: 1rem;
  }

  .form-group label {
    display: block;
    font-size: 0.85rem;
    color: var(--text-muted);
    margin-bottom: 0.5rem;
  }

  .form-group input[type="text"],
  .form-group input[type="password"],
  .form-group input[type="number"] {
    width: 100%;
    padding: 0.75rem 1rem;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 0.5rem;
    color: var(--text);
    font-size: 0.95rem;
    outline: none;
    transition: border-color 0.2s;
  }

  .form-group input:focus {
    border-color: var(--accent);
  }

  .form-hint {
    display: block;
    font-size: 0.75rem;
    color: var(--text-muted);
    margin-top: 0.25rem;
  }

  .checkbox-group label {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    cursor: pointer;
    font-size: 0.95rem;
    color: var(--text);
  }

  .checkbox-group input[type="checkbox"] {
    width: 1.1rem;
    height: 1.1rem;
    accent-color: var(--accent);
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

  .toast-container {
    position: fixed;
    top: 1.5rem;
    right: 1.5rem;
    z-index: 1000;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    pointer-events: none;
  }

  .toast {
    padding: 0.75rem 1.25rem;
    border-radius: 0.5rem;
    font-size: 0.9rem;
    font-weight: 500;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.3);
    animation: toast-in 0.35s ease-out;
    pointer-events: auto;
  }

  .toast-dismissing {
    animation: toast-out 0.4s ease-in forwards;
  }

  .toast-success {
    background: rgba(34, 197, 94, 0.15);
    border: 1px solid var(--green);
    color: var(--green);
  }

  .toast-error {
    background: rgba(239, 68, 68, 0.15);
    border: 1px solid var(--red);
    color: var(--red);
  }

  @keyframes toast-in {
    from {
      opacity: 0;
      transform: translateY(-0.75rem);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }

  @keyframes toast-out {
    from {
      opacity: 1;
      transform: translateY(0);
    }
    to {
      opacity: 0;
      transform: translateY(-0.75rem);
    }
  }
</style>
