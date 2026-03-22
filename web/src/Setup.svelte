<script lang="ts">
  import LogoBrand from './LogoBrand.svelte';
  import { apiPost } from './api';

  let { onSetup }: { onSetup: () => void } = $props();

  let adminPassword = $state('');
  let adminPasswordConfirm = $state('');
  let readPassword = $state('');
  let readPasswordConfirm = $state('');
  let singlePassword = $state(true);
  let error = $state<string | null>(null);
  let loading = $state(false);

  async function handleSetup(e: Event) {
    e.preventDefault();
    error = null;

    if (adminPassword !== adminPasswordConfirm) {
      error = 'Passwords do not match';
      return;
    }
    if (!singlePassword && readPassword !== readPasswordConfirm) {
      error = 'Read-only passwords do not match';
      return;
    }

    loading = true;
    try {
      const body: any = { admin_password: adminPassword };
      if (!singlePassword && readPassword) {
        body.read_password = readPassword;
      }

      const data = await apiPost<{ success: boolean; error?: string }>('/setup', body);
      if (data.success) {
        onSetup();
      } else {
        error = data.error || 'Setup failed';
      }
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  let canSubmit = $derived(
    !loading &&
    adminPassword.length > 0 &&
    adminPasswordConfirm.length > 0 &&
    (singlePassword || (readPassword.length > 0 && readPasswordConfirm.length > 0))
  );
</script>

<div class="setup-container">
  <div class="setup-card">
    <div class="setup-logo">
      <LogoBrand size="large" />
    </div>
    <p class="setup-subtitle">
      Set up your access password to get started.
    </p>

    <form onsubmit={handleSetup}>
      {#if error}
        <div class="error-msg">{error}</div>
      {/if}

      <div class="form-section">
        <h3>{singlePassword ? 'Password' : 'Admin Password'}</h3>
        {#if !singlePassword}
          <p class="form-hint">Used to access the admin panel and configure integrations.</p>
        {/if}
        <div class="form-group">
          <input
            type="password"
            bind:value={adminPassword}
            placeholder={singlePassword ? 'Choose a password' : 'Admin password'}
            disabled={loading}
            autofocus
          />
        </div>
        <div class="form-group">
          <input
            type="password"
            bind:value={adminPasswordConfirm}
            placeholder="Confirm password"
            disabled={loading}
          />
        </div>
      </div>

      <div class="form-group toggle-group">
        <label>
          <input
            type="checkbox"
            bind:checked={singlePassword}
            disabled={loading}
          />
          <span>Use one password for everything</span>
        </label>
        <p class="form-hint">
          {singlePassword
            ? 'Everyone with the password gets full admin access.'
            : 'Set a separate read-only password for view-only access.'}
        </p>
      </div>

      {#if !singlePassword}
        <div class="form-section">
          <h3>Read-Only Password</h3>
          <p class="form-hint">Allows viewing wishlist data without admin access.</p>
          <div class="form-group">
            <input
              type="password"
              bind:value={readPassword}
              placeholder="Read-only password"
              disabled={loading}
            />
          </div>
          <div class="form-group">
            <input
              type="password"
              bind:value={readPasswordConfirm}
              placeholder="Confirm read-only password"
              disabled={loading}
            />
          </div>
        </div>
      {/if}

      <button
        type="submit"
        class="setup-btn"
        disabled={!canSubmit}
      >
        {loading ? 'Setting up...' : 'Complete Setup'}
      </button>
    </form>
  </div>
</div>

<style>
  .setup-container {
    display: flex;
    justify-content: center;
    align-items: center;
    min-height: 80vh;
  }

  .setup-card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 1rem;
    padding: 2.5rem;
    width: 100%;
    max-width: 480px;
  }

  .setup-logo {
    display: flex;
    justify-content: center;
    margin-bottom: 1.5rem;
  }

  .setup-subtitle {
    text-align: center;
    color: var(--text-muted);
    margin-bottom: 2rem;
    font-size: 0.9rem;
    line-height: 1.5;
  }

  .form-section {
    margin-bottom: 1.5rem;
  }

  .form-section h3 {
    font-size: 1rem;
    font-weight: 600;
    margin-bottom: 0.25rem;
  }

  .form-hint {
    font-size: 0.8rem;
    color: var(--text-muted);
    margin-bottom: 0.75rem;
  }

  .form-group {
    margin-bottom: 0.75rem;
  }

  .form-group input[type="password"] {
    width: 100%;
    padding: 0.75rem 1rem;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 0.5rem;
    color: var(--text);
    font-size: 1rem;
    outline: none;
    transition: border-color 0.2s;
  }

  .form-group input[type="password"]:focus {
    border-color: var(--accent);
  }

  .toggle-group {
    margin-bottom: 1.5rem;
  }

  .toggle-group label {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    cursor: pointer;
    font-size: 0.95rem;
    color: var(--text);
    margin-bottom: 0.25rem;
  }

  .toggle-group input[type="checkbox"] {
    width: 1.1rem;
    height: 1.1rem;
    accent-color: var(--accent);
  }

  .setup-btn {
    width: 100%;
    padding: 0.75rem;
    background: var(--accent);
    color: white;
    border: none;
    border-radius: 0.5rem;
    font-size: 1rem;
    font-weight: 600;
    cursor: pointer;
    transition: opacity 0.2s;
    margin-top: 0.5rem;
  }

  .setup-btn:hover:not(:disabled) {
    opacity: 0.9;
  }

  .setup-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .error-msg {
    background: rgba(239, 68, 68, 0.1);
    border: 1px solid var(--red);
    border-radius: 0.5rem;
    padding: 0.75rem 1rem;
    margin-bottom: 1.25rem;
    color: var(--red);
    font-size: 0.9rem;
  }
</style>
