//! Behavioral Violation Tracker — aggregates violations from all guard systems.
//!
//! Collects violations from loop guard, taint checking, approval denials,
//! tool policy blocks, and other security systems into a per-agent log.
//! When an agent's weighted violation score exceeds a configurable threshold
//! within a time window, the tracker signals for autonomy downgrade.
//!
//! Thread-safe: all operations are lock-free via DashMap.

use dashmap::DashMap;
use openfang_types::violation::{ViolationConfig, ViolationKind, ViolationRecord};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

/// Thread-safe violation tracker shared across the kernel.
#[derive(Debug, Clone)]
pub struct ViolationTracker {
    /// Per-agent violation records, keyed by agent_id string.
    records: Arc<DashMap<String, Vec<ViolationRecord>>>,
    config: ViolationConfig,
}

impl ViolationTracker {
    /// Create a new tracker with default config.
    pub fn new() -> Self {
        Self {
            records: Arc::new(DashMap::new()),
            config: ViolationConfig::default(),
        }
    }

    /// Create a tracker with custom config.
    pub fn with_config(config: ViolationConfig) -> Self {
        Self {
            records: Arc::new(DashMap::new()),
            config,
        }
    }

    /// Record a violation for an agent.
    ///
    /// Returns the current weighted score for the agent within the time window.
    /// If the score exceeds `max_score`, the caller should downgrade the agent.
    pub fn record(
        &self,
        agent_id: &str,
        kind: ViolationKind,
        tool_name: Option<&str>,
        detail: &str,
    ) -> u32 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let record = ViolationRecord {
            kind,
            agent_id: agent_id.to_string(),
            tool_name: tool_name.map(String::from),
            detail: detail.to_string(),
            timestamp: now,
        };

        let severity = kind.severity();
        let label = kind.label();

        let mut entry = self.records.entry(agent_id.to_string()).or_default();
        entry.push(record);

        // Trim old records beyond max_records
        if entry.len() > self.config.max_records {
            let excess = entry.len() - self.config.max_records;
            entry.drain(..excess);
        }

        // Calculate weighted score within time window
        let cutoff = now.saturating_sub(self.config.window_secs);
        let score: u32 = entry
            .iter()
            .filter(|r| r.timestamp >= cutoff)
            .map(|r| r.kind.severity())
            .sum();

        if score >= self.config.max_score {
            warn!(
                agent_id,
                kind = label,
                score,
                threshold = self.config.max_score,
                "Violation score exceeds threshold — agent should be downgraded"
            );
        } else {
            info!(
                agent_id,
                kind = label,
                severity,
                score,
                tool = tool_name.unwrap_or("-"),
                "Behavioral violation recorded"
            );
        }

        score
    }

    /// Check whether an agent should be auto-downgraded based on violation score.
    pub fn should_downgrade(&self, agent_id: &str) -> bool {
        if !self.config.auto_downgrade {
            return false;
        }
        self.current_score(agent_id) >= self.config.max_score
    }

    /// Get the current weighted violation score for an agent within the time window.
    pub fn current_score(&self, agent_id: &str) -> u32 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let cutoff = now.saturating_sub(self.config.window_secs);

        self.records
            .get(agent_id)
            .map(|entry| {
                entry
                    .iter()
                    .filter(|r| r.timestamp >= cutoff)
                    .map(|r| r.kind.severity())
                    .sum()
            })
            .unwrap_or(0)
    }

    /// Get all violation records for an agent.
    pub fn get_records(&self, agent_id: &str) -> Vec<ViolationRecord> {
        self.records
            .get(agent_id)
            .map(|entry| entry.clone())
            .unwrap_or_default()
    }

    /// Get recent violations for an agent (within the scoring window).
    pub fn recent_records(&self, agent_id: &str) -> Vec<ViolationRecord> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let cutoff = now.saturating_sub(self.config.window_secs);

        self.records
            .get(agent_id)
            .map(|entry| {
                entry
                    .iter()
                    .filter(|r| r.timestamp >= cutoff)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get violation summary for all agents (for dashboard).
    pub fn summary_all(&self) -> Vec<AgentViolationSummary> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let cutoff = now.saturating_sub(self.config.window_secs);

        self.records
            .iter()
            .map(|entry| {
                let agent_id = entry.key().clone();
                let all = entry.value();
                let recent: Vec<&ViolationRecord> =
                    all.iter().filter(|r| r.timestamp >= cutoff).collect();
                let score: u32 = recent.iter().map(|r| r.kind.severity()).sum();

                // Count by kind
                let mut kind_counts = std::collections::HashMap::new();
                for r in &recent {
                    *kind_counts.entry(r.kind).or_insert(0u32) += 1;
                }

                AgentViolationSummary {
                    agent_id,
                    total_violations: all.len() as u32,
                    recent_violations: recent.len() as u32,
                    weighted_score: score,
                    threshold: self.config.max_score,
                    should_downgrade: self.config.auto_downgrade && score >= self.config.max_score,
                    top_kinds: kind_counts,
                }
            })
            .collect()
    }

    /// Clear violation records for an agent (e.g., after manual review).
    pub fn clear(&self, agent_id: &str) {
        self.records.remove(agent_id);
    }

    /// Get the tracker configuration.
    pub fn config(&self) -> &ViolationConfig {
        &self.config
    }
}

impl Default for ViolationTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary of violations for a single agent.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentViolationSummary {
    pub agent_id: String,
    pub total_violations: u32,
    pub recent_violations: u32,
    pub weighted_score: u32,
    pub threshold: u32,
    pub should_downgrade: bool,
    pub top_kinds: std::collections::HashMap<ViolationKind, u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_score() {
        let tracker = ViolationTracker::new();
        let score = tracker.record("agent-1", ViolationKind::LoopGuard, Some("shell_exec"), "Blocked repeated call");
        assert_eq!(score, ViolationKind::LoopGuard.severity());
    }

    #[test]
    fn multiple_violations_accumulate() {
        let tracker = ViolationTracker::new();
        tracker.record("agent-1", ViolationKind::LoopGuard, None, "v1");
        tracker.record("agent-1", ViolationKind::ApprovalDenied, None, "v2");
        let score = tracker.current_score("agent-1");
        assert_eq!(
            score,
            ViolationKind::LoopGuard.severity() + ViolationKind::ApprovalDenied.severity()
        );
    }

    #[test]
    fn different_agents_independent() {
        let tracker = ViolationTracker::new();
        tracker.record("agent-1", ViolationKind::ShellInjection, None, "bad");
        tracker.record("agent-2", ViolationKind::LoopGuard, None, "ok");
        assert_eq!(tracker.current_score("agent-1"), ViolationKind::ShellInjection.severity());
        assert_eq!(tracker.current_score("agent-2"), ViolationKind::LoopGuard.severity());
    }

    #[test]
    fn should_downgrade_threshold() {
        let config = ViolationConfig {
            max_score: 10,
            window_secs: 3600,
            auto_downgrade: true,
            max_records: 200,
        };
        let tracker = ViolationTracker::with_config(config);
        // ShellInjection severity = 10, which meets threshold
        tracker.record("agent-1", ViolationKind::ShellInjection, None, "injection detected");
        assert!(tracker.should_downgrade("agent-1"));
    }

    #[test]
    fn auto_downgrade_disabled() {
        let config = ViolationConfig {
            max_score: 10,
            auto_downgrade: false,
            ..Default::default()
        };
        let tracker = ViolationTracker::with_config(config);
        tracker.record("agent-1", ViolationKind::ShellInjection, None, "injection");
        assert!(!tracker.should_downgrade("agent-1"));
    }

    #[test]
    fn get_records() {
        let tracker = ViolationTracker::new();
        tracker.record("agent-1", ViolationKind::ToolTimeout, Some("web_fetch"), "timed out");
        let records = tracker.get_records("agent-1");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, ViolationKind::ToolTimeout);
        assert_eq!(records[0].tool_name, Some("web_fetch".to_string()));
    }

    #[test]
    fn summary_all() {
        let tracker = ViolationTracker::new();
        tracker.record("agent-1", ViolationKind::LoopGuard, None, "v1");
        tracker.record("agent-1", ViolationKind::LoopGuard, None, "v2");
        tracker.record("agent-2", ViolationKind::ApprovalDenied, None, "v3");
        let summary = tracker.summary_all();
        assert_eq!(summary.len(), 2);
    }

    #[test]
    fn clear_removes_records() {
        let tracker = ViolationTracker::new();
        tracker.record("agent-1", ViolationKind::LoopGuard, None, "v1");
        assert_eq!(tracker.current_score("agent-1"), ViolationKind::LoopGuard.severity());
        tracker.clear("agent-1");
        assert_eq!(tracker.current_score("agent-1"), 0);
    }

    #[test]
    fn max_records_trimming() {
        let config = ViolationConfig {
            max_records: 5,
            ..Default::default()
        };
        let tracker = ViolationTracker::with_config(config);
        for i in 0..10 {
            tracker.record("agent-1", ViolationKind::LoopGuard, None, &format!("v{i}"));
        }
        assert_eq!(tracker.get_records("agent-1").len(), 5);
    }

    #[test]
    fn unknown_agent_returns_defaults() {
        let tracker = ViolationTracker::new();
        assert_eq!(tracker.current_score("nonexistent"), 0);
        assert!(tracker.get_records("nonexistent").is_empty());
        assert!(!tracker.should_downgrade("nonexistent"));
    }
}
