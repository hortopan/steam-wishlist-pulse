<script lang="ts">
  import { onMount } from "svelte";
  import LogoBrand from "./LogoBrand.svelte";
  import { apiPost } from "./api";

  let { onLogin }: { onLogin: () => void } = $props();

  let password = $state("");
  let error = $state<string | null>(null);
  let loading = $state(false);
  let passwordInput!: HTMLInputElement;

  onMount(() => {
    passwordInput?.focus();
  });

  async function handleLogin(e: Event) {
    e.preventDefault();
    loading = true;
    error = null;
    try {
      const data = await apiPost<{ success: boolean; error?: string }>("/auth/login", { password });
      if (data.success) {
        // Verify the session cookie was actually stored by the browser
        const status = await fetch('/api/auth/status');
        const statusData = await status.json();
        if (statusData.authenticated) {
          onLogin();
        } else {
          error = window.location.protocol === 'http:'
            ? "Login succeeded but your browser rejected the session cookie because you're on plain HTTP. Either access the app over HTTPS, or start the server with the --insecure flag to allow cookies over HTTP."
            : "Login succeeded but session could not be established. Your browser may be blocking cookies.";
        }
      } else {
        error = data.error || "Login failed";
      }
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
  }
</script>

<div class="login-container">
  <div class="login-card">
    <div class="login-logo">
      <LogoBrand size="large" />
    </div>

    <form onsubmit={handleLogin}>
      {#if error}
        <div class="error-msg">{error}</div>
      {/if}

      <div class="form-group">
        <label for="password">Password</label>
        <input
          id="password"
          type="password"
          bind:value={password}
          bind:this={passwordInput}
          placeholder="Enter your password"
          disabled={loading}
        />
      </div>

      <button type="submit" class="login-btn" disabled={loading || !password}>
        {loading ? "Signing in..." : "Sign In"}
      </button>
    </form>
  </div>
</div>

<style>
  .login-container {
    display: flex;
    justify-content: center;
    align-items: center;
    min-height: 80vh;
  }

  .login-card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 1rem;
    padding: 3rem;
    width: 100%;
    max-width: 400px;
  }

  @media (max-width: 600px) {
    .login-card {
      padding: 1.5rem;
    }
  }

  .login-logo {
    display: flex;
    justify-content: center;
    margin-bottom: 2rem;
  }

  .form-group {
    margin-bottom: 1.5rem;
  }

  .form-group label {
    display: block;
    font-size: 0.85rem;
    color: var(--text-muted);
    margin-bottom: 0.5rem;
  }

  .form-group input {
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

  .form-group input:focus {
    border-color: var(--accent);
  }

  .login-btn {
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
  }

  .login-btn:hover:not(:disabled) {
    opacity: 0.9;
  }

  .login-btn:disabled {
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
