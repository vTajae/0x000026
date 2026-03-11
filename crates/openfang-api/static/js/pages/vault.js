// OpenFang Vault Page — Credential management dashboard
// Provides UI for viewing, adding, and removing credentials stored in the dotenv chain.

function vaultPage() {
  return {
    keys: [],
    vaultStatus: null,
    loadError: '',
    newKey: '',
    newValue: '',
    saving: false,
    deleteConfirm: '',

    async loadData() {
      this.loadError = '';
      try {
        var [statusResp, keysResp] = await Promise.all([
          OpenFangAPI.get('/api/vault/status'),
          OpenFangAPI.get('/api/vault/keys'),
        ]);
        this.vaultStatus = statusResp;
        this.keys = keysResp.keys || [];
      } catch (e) {
        this.loadError = e.message || 'Could not load vault data.';
      }
    },

    get configuredCount() {
      return this.keys.filter(function(k) { return k.available; }).length;
    },

    get missingCount() {
      return this.keys.filter(function(k) { return !k.available; }).length;
    },

    async saveKey() {
      if (!this.newKey || !this.newValue) return;
      this.saving = true;
      try {
        await OpenFangAPI.post('/api/vault/set', {
          key: this.newKey,
          value: this.newValue,
        });
        this.newKey = '';
        this.newValue = '';
        await this.loadData();
      } catch (e) {
        alert('Failed to save: ' + (e.message || e));
      }
      this.saving = false;
    },

    async deleteKey(key) {
      if (this.deleteConfirm !== key) {
        this.deleteConfirm = key;
        return;
      }
      try {
        await OpenFangAPI.post('/api/vault/delete', { key: key });
        this.deleteConfirm = '';
        await this.loadData();
      } catch (e) {
        alert('Failed to delete: ' + (e.message || e));
      }
    },

    cancelDelete() {
      this.deleteConfirm = '';
    },
  };
}
