<script lang="ts">
  import { apiPost, AuthError } from './api';

  let {
    onLogout,
    showToast,
  }: {
    onLogout: () => void;
    showToast: (type: 'success' | 'error', text: string) => void;
  } = $props();

  let currentPassword = $state('');
  let newAdminPassword = $state('');
  let newReadPassword = $state('');
  let savingPassword = $state(false);

  async function changePassword(e: Event) {
    e.preventDefault();
    savingPassword = true;
    try {
      const body: any = { current_password: currentPassword };
      if (newAdminPassword) body.new_admin_password = newAdminPassword;
      if (newReadPassword) body.new_read_password = newReadPassword;

      const data = await apiPost<{ success: boolean; error?: string }>('/admin/change-password', body);
      if (data.success) {
        showToast('success', 'Passwords updated successfully');
        currentPassword = '';
        newAdminPassword = '';
        newReadPassword = '';
      } else {
        showToast('error', data.error || 'Failed to change password');
      }
    } catch (e: any) {
      if (e instanceof AuthError) { onLogout(); return; }
      showToast('error', e.message);
    } finally {
      savingPassword = false;
    }
  }
</script>

<section class="config-section">
  <h2>Change Passwords</h2>
  <p class="section-desc">Update admin or read-only access passwords.</p>
  <form onsubmit={changePassword}>
    <div class="form-group">
      <label for="current-pw">Current Admin Password</label>
      <input
        id="current-pw"
        type="password"
        bind:value={currentPassword}
        placeholder="Current admin password"
        disabled={savingPassword}
      />
    </div>

    <div class="form-group">
      <label for="new-admin-pw">New Admin Password</label>
      <input
        id="new-admin-pw"
        type="password"
        bind:value={newAdminPassword}
        placeholder="Leave blank to keep current"
        disabled={savingPassword}
      />
      <span class="form-hint"
        >Full access — can add/remove games, change settings, and manage
        passwords.</span
      >
    </div>

    <div class="form-group">
      <label for="new-read-pw">New Read-Only Password</label>
      <input
        id="new-read-pw"
        type="password"
        bind:value={newReadPassword}
        placeholder="Leave blank to keep current"
        disabled={savingPassword}
      />
      <span class="form-hint"
        >View-only access — can see the dashboard and tracking data but
        cannot make any changes.</span
      >
    </div>

    <button
      type="submit"
      class="save-btn"
      disabled={savingPassword ||
        !currentPassword ||
        (!newAdminPassword && !newReadPassword)}
    >
      {savingPassword ? 'Updating...' : 'Update Passwords'}
    </button>
  </form>
</section>

<style>
  .config-section {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 0.75rem;
    padding: 1.5rem;
  }

  .config-section h2 {
    font-size: 1.2rem;
    font-weight: 600;
    margin-bottom: 0.25rem;
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

  .form-group input[type="password"] {
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
</style>
