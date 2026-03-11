// OpenFang Workflows Page — Workflow builder + run history
'use strict';

function workflowsPage() {
  return {
    // -- Workflows state --
    workflows: [],
    showCreateModal: false,
    runModal: null,
    runInput: '',
    runResult: '',
    running: false,
    loading: true,
    loadError: '',
    newWf: { name: '', description: '', steps: [{ name: '', agent_name: '', mode: 'sequential', prompt: '{{input}}' }] },

    // -- Workflows methods --
    async loadWorkflows() {
      this.loading = true;
      this.loadError = '';
      try {
        this.workflows = await OpenFangAPI.get('/api/workflows');
      } catch(e) {
        this.workflows = [];
        this.loadError = e.message || 'Could not load workflows.';
      }
      this.loading = false;
    },

    async loadData() { return this.loadWorkflows(); },

    async createWorkflow() {
      var steps = this.newWf.steps.map(function(s) {
        return { name: s.name || 'step', agent_name: s.agent_name, mode: s.mode, prompt: s.prompt || '{{input}}' };
      });
      try {
        var wfName = this.newWf.name;
        await OpenFangAPI.post('/api/workflows', { name: wfName, description: this.newWf.description, steps: steps });
        this.showCreateModal = false;
        this.newWf = { name: '', description: '', steps: [{ name: '', agent_name: '', mode: 'sequential', prompt: '{{input}}' }] };
        OpenFangToast.success('Workflow "' + wfName + '" created');
        await this.loadWorkflows();
      } catch(e) {
        OpenFangToast.error('Failed to create workflow: ' + e.message);
      }
    },

    showRunModal(wf) {
      this.runModal = wf;
      this.runInput = '';
      this.runResult = '';
    },

    async executeWorkflow() {
      if (!this.runModal) return;
      this.running = true;
      this.runResult = '';
      try {
        var res = await OpenFangAPI.post('/api/workflows/' + this.runModal.id + '/run', { input: this.runInput });
        this.runResult = res.output || JSON.stringify(res, null, 2);
        OpenFangToast.success('Workflow completed');
      } catch(e) {
        this.runResult = 'Error: ' + e.message;
        OpenFangToast.error('Workflow failed: ' + e.message);
      }
      this.running = false;
    },

    async viewRuns(wf) {
      try {
        var runs = await OpenFangAPI.get('/api/workflows/' + wf.id + '/runs');
        this.runResult = JSON.stringify(runs, null, 2);
        this.runModal = wf;
      } catch(e) {
        OpenFangToast.error('Failed to load run history: ' + e.message);
      }
    },

    // -- Templates --
    templates: [],
    showTemplateModal: false,
    selectedTemplate: null,
    templateAgents: {},

    async loadTemplates() {
      try {
        var data = await OpenFangAPI.get('/api/workflows/templates');
        this.templates = data.templates || [];
      } catch(e) {
        this.templates = [];
      }
    },

    selectTemplate(tpl) {
      this.selectedTemplate = tpl;
      this.templateAgents = {};
      var self = this;
      tpl.required_roles.forEach(function(role) {
        self.templateAgents[role] = '';
      });
      this.showTemplateModal = true;
    },

    async instantiateTemplate() {
      if (!this.selectedTemplate) return;
      try {
        var res = await OpenFangAPI.post('/api/workflows/templates/instantiate', {
          template_id: this.selectedTemplate.id,
          agents: this.templateAgents
        });
        this.showTemplateModal = false;
        OpenFangToast.success('Workflow "' + this.selectedTemplate.name + '" created');
        await this.loadWorkflows();
      } catch(e) {
        OpenFangToast.error('Failed to instantiate: ' + e.message);
      }
    },

    // -- EARS Requirements --
    earsInput: '',
    earsResults: null,

    async parseEars() {
      if (!this.earsInput.trim()) return;
      var lines = this.earsInput.trim().split('\n').filter(function(l) { return l.trim(); });
      var requirements = lines.map(function(line, i) {
        return { id: 'REQ-' + String(i + 1).padStart(3, '0'), text: line.trim() };
      });
      try {
        this.earsResults = await OpenFangAPI.post('/api/ears/parse', { requirements: requirements });
      } catch(e) {
        OpenFangToast.error('EARS parse failed: ' + e.message);
      }
    },

    earsPatternLabel(req) {
      if (!req || !req.pattern) return 'freeform';
      return req.pattern.type || 'freeform';
    },

    earsPatternColor(type) {
      var colors = {
        'ubiquitous': '#3b82f6',
        'event_driven': '#10b981',
        'state_driven': '#f59e0b',
        'unwanted_behavior': '#ef4444',
        'optional': '#8b5cf6',
        'complex': '#ec4899',
        'freeform': '#6b7280'
      };
      return colors[type] || '#6b7280';
    }
  };
}
