// OpenFang Quality Monitoring Page — Assertions, self-critique, and EARS tracking
'use strict';

function qualityPage() {
  return {
    tab: 'assertions',
    agents: [],
    agentAssertions: {},
    loading: true,
    loadError: '',

    // Critique state
    critiqueQuery: '',
    critiqueResponse: '',
    critiqueResult: null,
    critiqueParsed: null,

    // EARS state
    earsSpecs: [],

    // PBT state
    pbtReqText: '',
    pbtInvariants: [],
    pbtCheckInput: '',
    pbtCheckResponse: '',
    pbtReport: null,

    // Curriculum state
    curriculumAgents: {},
    curriculumTiers: ['Foundational', 'Intermediate', 'Advanced', 'Expert'],

    // Reflection state
    insightAgents: {},

    async loadData() {
      this.loading = true;
      this.loadError = '';
      try {
        await Promise.all([
          this.loadAgents(),
          this.loadViolations()
        ]);
      } catch(e) {
        this.loadError = e.message || 'Could not load quality data.';
      }
      this.loading = false;
    },

    async loadAgents() {
      try {
        this.agents = await OpenFangAPI.get('/api/agents');
      } catch(e) {
        this.agents = [];
      }
    },

    violations: [],
    violationConfig: {},

    async loadViolations() {
      try {
        var data = await OpenFangAPI.get('/api/violations');
        this.violations = data.agents || [];
        this.violationConfig = data.config || {};
      } catch(e) {
        this.violations = [];
      }
    },

    async loadAssertionsForAgent(agentId) {
      try {
        var data = await OpenFangAPI.get('/api/agents/' + agentId + '/assertions');
        this.agentAssertions[agentId] = data.assertions || [];
      } catch(e) {
        this.agentAssertions[agentId] = [];
      }
    },

    async refreshAssertions(agentId) {
      try {
        await OpenFangAPI.del('/api/agents/' + agentId + '/assertions/cache');
        await this.loadAssertionsForAgent(agentId);
        OpenFangToast.success('Assertions reloaded');
      } catch(e) {
        OpenFangToast.error('Failed to refresh: ' + e.message);
      }
    },

    getAssertions(agentId) {
      return this.agentAssertions[agentId] || [];
    },

    async loadAllAssertions() {
      var self = this;
      var promises = this.agents.map(function(a) {
        return self.loadAssertionsForAgent(a.id);
      });
      await Promise.all(promises);
    },

    totalAssertions() {
      var total = 0;
      var self = this;
      Object.keys(this.agentAssertions).forEach(function(k) {
        total += (self.agentAssertions[k] || []).length;
      });
      return total;
    },

    agentsWithAssertions() {
      var self = this;
      return this.agents.filter(function(a) {
        return (self.agentAssertions[a.id] || []).length > 0;
      });
    },

    // Critique helpers
    async buildCritique() {
      if (!this.critiqueQuery.trim() || !this.critiqueResponse.trim()) return;
      try {
        this.critiqueResult = await OpenFangAPI.post('/api/critique', {
          query: this.critiqueQuery,
          response: this.critiqueResponse
        });
      } catch(e) {
        OpenFangToast.error('Critique failed: ' + e.message);
      }
    },

    async parseCritique(text) {
      try {
        this.critiqueParsed = await OpenFangAPI.post('/api/critique/parse', {
          critique_response: text
        });
      } catch(e) {
        OpenFangToast.error('Parse failed: ' + e.message);
      }
    },

    // Check assertions manually
    async checkManual(assertionsList, response) {
      try {
        return await OpenFangAPI.post('/api/assertions/check', {
          assertions: assertionsList,
          response: response,
          tool_call_count: 0,
          cost_usd: 0
        });
      } catch(e) {
        OpenFangToast.error('Check failed: ' + e.message);
        return null;
      }
    },

    conditionLabel(condition) {
      if (!condition || !condition.type) return 'unknown';
      return condition.type.replace(/_/g, ' ');
    },

    failActionColor(action) {
      var colors = {
        'warn': '#f59e0b',
        'violate': '#ef4444',
        'block': '#dc2626',
        'review': '#8b5cf6'
      };
      return colors[action] || '#6b7280';
    },

    violationScoreColor(score) {
      var threshold = this.violationConfig.max_score || 50;
      var pct = score / threshold;
      if (pct >= 1.0) return '#ef4444';
      if (pct >= 0.7) return '#f97316';
      if (pct >= 0.4) return '#eab308';
      return '#22c55e';
    },

    violationScoreWidth(score) {
      var threshold = this.violationConfig.max_score || 50;
      return Math.min(100, Math.round((score / threshold) * 100)) + '%';
    },

    // PBT methods
    async generateInvariants() {
      if (!this.pbtReqText.trim()) return;
      try {
        var data = await OpenFangAPI.post('/api/pbt/generate', { requirements_text: this.pbtReqText });
        this.pbtInvariants = data.invariants || [];
        OpenFangToast.success('Generated ' + this.pbtInvariants.length + ' invariants');
      } catch(e) {
        OpenFangToast.error('PBT generate failed: ' + e.message);
      }
    },

    async checkInvariants() {
      if (!this.pbtInvariants.length || !this.pbtCheckResponse.trim()) return;
      try {
        this.pbtReport = await OpenFangAPI.post('/api/pbt/check', {
          invariants: this.pbtInvariants,
          input: this.pbtCheckInput,
          response: this.pbtCheckResponse
        });
      } catch(e) {
        OpenFangToast.error('PBT check failed: ' + e.message);
      }
    },

    severityColor(severity) {
      var colors = { 'Critical': '#ef4444', 'Warning': '#f59e0b', 'Info': '#3b82f6' };
      return colors[severity] || '#6b7280';
    },

    // Curriculum methods
    async loadCurriculumForAgent(agentId) {
      try {
        this.curriculumAgents[agentId] = await OpenFangAPI.get('/api/agents/' + agentId + '/curriculum');
      } catch(e) {
        this.curriculumAgents[agentId] = null;
      }
    },

    async loadAllCurriculum() {
      var self = this;
      await Promise.all(this.agents.map(function(a) { return self.loadCurriculumForAgent(a.id); }));
    },

    getCurriculum(agentId) {
      return this.curriculumAgents[agentId] || null;
    },

    tierColor(tier) {
      var colors = { 'Foundational': '#6b7280', 'Intermediate': '#3b82f6', 'Advanced': '#f59e0b', 'Expert': '#10b981' };
      return colors[tier] || '#6b7280';
    },

    masteryWidth(mastery) {
      return Math.round((mastery || 0) * 100) + '%';
    },

    // Reflection methods
    async loadInsightsForAgent(agentId) {
      try {
        this.insightAgents[agentId] = await OpenFangAPI.get('/api/agents/' + agentId + '/insights');
      } catch(e) {
        this.insightAgents[agentId] = null;
      }
    },

    async loadAllInsights() {
      var self = this;
      await Promise.all(this.agents.map(function(a) { return self.loadInsightsForAgent(a.id); }));
    },

    getInsights(agentId) {
      var data = this.insightAgents[agentId];
      return data ? (data.insights || []) : [];
    },

    confidenceColor(c) {
      if (c >= 80) return '#10b981';
      if (c >= 50) return '#f59e0b';
      return '#ef4444';
    }
  };
}
