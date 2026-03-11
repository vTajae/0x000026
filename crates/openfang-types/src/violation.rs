//! Behavioral Violation Tracking — unified violation records from all guard systems.
//!
//! Aggregates violations from loop guard, taint checking, approval denials,
//! tool policy blocks, and shell injection detection into a single queryable
//! record. Enables auto-downgrade of agent autonomy when violation thresholds
//! are exceeded.

use serde::{Deserialize, Serialize};

/// Category of behavioral violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViolationKind {
    /// Loop guard blocked or circuit-broke a repeated tool call.
    LoopGuard,
    /// Taint tracking detected potential data flow violation.
    TaintFlow,
    /// Approval system denied a tool call.
    ApprovalDenied,
    /// Tool policy blocked access to a tool.
    ToolPolicyBlock,
    /// Shell injection metacharacters detected.
    ShellInjection,
    /// Prompt injection markers detected in input.
    PromptInjection,
    /// Resource quota exceeded (tokens, cost, memory).
    QuotaExceeded,
    /// SSRF attempt blocked.
    SsrfBlocked,
    /// Tool execution timed out.
    ToolTimeout,
    /// Environment variable leak detected.
    EnvLeak,
}

impl ViolationKind {
    /// Human-readable label for display.
    pub fn label(&self) -> &'static str {
        match self {
            Self::LoopGuard => "Loop Guard",
            Self::TaintFlow => "Taint Flow",
            Self::ApprovalDenied => "Approval Denied",
            Self::ToolPolicyBlock => "Tool Policy Block",
            Self::ShellInjection => "Shell Injection",
            Self::PromptInjection => "Prompt Injection",
            Self::QuotaExceeded => "Quota Exceeded",
            Self::SsrfBlocked => "SSRF Blocked",
            Self::ToolTimeout => "Tool Timeout",
            Self::EnvLeak => "Env Leak",
        }
    }

    /// Severity weight (1-10) for violation budget calculation.
    pub fn severity(&self) -> u32 {
        match self {
            Self::ShellInjection => 10,
            Self::PromptInjection => 9,
            Self::SsrfBlocked => 8,
            Self::TaintFlow => 7,
            Self::ApprovalDenied => 5,
            Self::ToolPolicyBlock => 4,
            Self::QuotaExceeded => 3,
            Self::LoopGuard => 3,
            Self::ToolTimeout => 2,
            Self::EnvLeak => 2,
        }
    }
}

/// A single violation event recorded by a guard system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViolationRecord {
    /// What kind of violation occurred.
    pub kind: ViolationKind,
    /// Agent that caused the violation.
    pub agent_id: String,
    /// Tool involved (if applicable).
    pub tool_name: Option<String>,
    /// Human-readable description of what happened.
    pub detail: String,
    /// Unix timestamp (seconds) when the violation occurred.
    pub timestamp: u64,
}

/// Configuration for the violation tracker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViolationConfig {
    /// Maximum weighted violation score before auto-downgrade.
    /// Score = sum of severity weights for all violations in the window.
    pub max_score: u32,
    /// Time window (seconds) for violation score calculation.
    /// Only violations within this window count toward the score.
    pub window_secs: u64,
    /// Whether auto-downgrade is enabled.
    pub auto_downgrade: bool,
    /// Maximum violation records to retain per agent.
    pub max_records: usize,
}

impl Default for ViolationConfig {
    fn default() -> Self {
        Self {
            max_score: 50,
            window_secs: 3600, // 1 hour
            auto_downgrade: true,
            max_records: 200,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_ordering() {
        // Shell injection and prompt injection should be highest severity
        assert!(ViolationKind::ShellInjection.severity() >= 8);
        assert!(ViolationKind::PromptInjection.severity() >= 8);
        // Timeouts and leaks should be low severity
        assert!(ViolationKind::ToolTimeout.severity() <= 3);
        assert!(ViolationKind::EnvLeak.severity() <= 3);
    }

    #[test]
    fn violation_kind_labels() {
        assert_eq!(ViolationKind::LoopGuard.label(), "Loop Guard");
        assert_eq!(ViolationKind::SsrfBlocked.label(), "SSRF Blocked");
    }

    #[test]
    fn default_config() {
        let config = ViolationConfig::default();
        assert_eq!(config.max_score, 50);
        assert_eq!(config.window_secs, 3600);
        assert!(config.auto_downgrade);
        assert_eq!(config.max_records, 200);
    }

    #[test]
    fn violation_record_serde_roundtrip() {
        let record = ViolationRecord {
            kind: ViolationKind::LoopGuard,
            agent_id: "test-agent".to_string(),
            tool_name: Some("shell_exec".to_string()),
            detail: "Repeated tool call blocked".to_string(),
            timestamp: 1234567890,
        };
        let json = serde_json::to_string(&record).unwrap();
        let parsed: ViolationRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.kind, record.kind);
        assert_eq!(parsed.agent_id, record.agent_id);
        assert_eq!(parsed.tool_name, record.tool_name);
    }

    #[test]
    fn violation_config_serde_roundtrip() {
        let config = ViolationConfig {
            max_score: 100,
            window_secs: 7200,
            auto_downgrade: false,
            max_records: 500,
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: ViolationConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.max_score, 100);
        assert_eq!(parsed.window_secs, 7200);
        assert!(!parsed.auto_downgrade);
    }
}
