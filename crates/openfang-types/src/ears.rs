//! EARS (Easy Approach to Requirements Syntax) requirement parser.
//!
//! Implements a subset of the EARS notation for machine-readable requirements:
//!
//! - **Ubiquitous**: "The <system> shall <action>."
//! - **Event-driven**: "When <event>, the <system> shall <action>."
//! - **State-driven**: "While <state>, the <system> shall <action>."
//! - **Unwanted behavior**: "If <condition>, then the <system> shall <action>."
//! - **Optional**: "Where <feature>, the <system> shall <action>."
//! - **Complex**: "While <state>, when <event>, the <system> shall <action>."
//!
//! Requirements are parsed from natural-language strings and stored as structured
//! types for validation, traceability, and injection into agent system prompts.

use serde::{Deserialize, Serialize};

/// A single EARS requirement with parsed structure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EarsRequirement {
    /// Unique requirement identifier (e.g., "REQ-001").
    pub id: String,
    /// The raw requirement text.
    pub text: String,
    /// Parsed EARS pattern.
    pub pattern: EarsPattern,
    /// Priority level.
    #[serde(default)]
    pub priority: RequirementPriority,
    /// Verification status.
    #[serde(default)]
    pub status: RequirementStatus,
    /// Optional tags for grouping.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// EARS requirement patterns — each maps to a different trigger style.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EarsPattern {
    /// "The <system> shall <action>."
    Ubiquitous {
        system: String,
        action: String,
    },
    /// "When <event>, the <system> shall <action>."
    EventDriven {
        event: String,
        system: String,
        action: String,
    },
    /// "While <state>, the <system> shall <action>."
    StateDriven {
        state: String,
        system: String,
        action: String,
    },
    /// "If <condition>, then the <system> shall <action>."
    UnwantedBehavior {
        condition: String,
        system: String,
        action: String,
    },
    /// "Where <feature>, the <system> shall <action>."
    Optional {
        feature: String,
        system: String,
        action: String,
    },
    /// "While <state>, when <event>, the <system> shall <action>."
    Complex {
        state: String,
        event: String,
        system: String,
        action: String,
    },
    /// Unparseable — stored as raw text.
    Freeform {
        text: String,
    },
}

/// Requirement priority levels.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RequirementPriority {
    Critical,
    #[default]
    High,
    Medium,
    Low,
}

/// Verification status of a requirement.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RequirementStatus {
    #[default]
    Pending,
    Verified,
    Failed,
    Deferred,
}

/// A collection of EARS requirements for an agent or project.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EarsSpec {
    /// Spec name.
    pub name: String,
    /// All requirements.
    pub requirements: Vec<EarsRequirement>,
}

impl EarsSpec {
    /// Create a new empty spec.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            requirements: Vec::new(),
        }
    }

    /// Add a requirement parsed from text.
    pub fn add(&mut self, id: impl Into<String>, text: impl Into<String>) -> &mut EarsRequirement {
        let text = text.into();
        let pattern = parse_ears_pattern(&text);
        self.requirements.push(EarsRequirement {
            id: id.into(),
            text,
            pattern,
            priority: RequirementPriority::default(),
            status: RequirementStatus::default(),
            tags: Vec::new(),
        });
        self.requirements.last_mut().unwrap()
    }

    /// Count requirements by status.
    pub fn count_by_status(&self, status: RequirementStatus) -> usize {
        self.requirements.iter().filter(|r| r.status == status).count()
    }

    /// Format requirements as markdown for injection into system prompts.
    pub fn to_prompt_markdown(&self) -> String {
        if self.requirements.is_empty() {
            return String::new();
        }
        let mut out = format!("## Requirements Spec: {}\n\n", self.name);
        for req in &self.requirements {
            let status_icon = match req.status {
                RequirementStatus::Pending => "[ ]",
                RequirementStatus::Verified => "[x]",
                RequirementStatus::Failed => "[!]",
                RequirementStatus::Deferred => "[-]",
            };
            let priority = match req.priority {
                RequirementPriority::Critical => " (CRITICAL)",
                RequirementPriority::High => "",
                RequirementPriority::Medium => " (medium)",
                RequirementPriority::Low => " (low)",
            };
            out.push_str(&format!(
                "- {} **{}**{}: {}\n",
                status_icon, req.id, priority, req.text
            ));
        }
        out
    }
}

/// Parse a natural-language requirement into an EARS pattern.
pub fn parse_ears_pattern(text: &str) -> EarsPattern {
    let trimmed = text.trim().trim_end_matches('.');

    // Complex: "While <state>, when <event>, the <system> shall <action>"
    if let Some(rest) = strip_prefix_ci(trimmed, "while ") {
        if let Some(comma_pos) = rest.find(',') {
            let state = rest[..comma_pos].trim().to_string();
            let after_comma = rest[comma_pos + 1..].trim();
            if let Some(when_rest) = strip_prefix_ci(after_comma, "when ") {
                if let Some((event, system, action)) = parse_event_system_action(when_rest) {
                    return EarsPattern::Complex {
                        state,
                        event,
                        system,
                        action,
                    };
                }
            }
            // State-driven: "While <state>, the <system> shall <action>"
            if let Some((system, action)) = parse_system_shall_action(after_comma) {
                return EarsPattern::StateDriven {
                    state,
                    system,
                    action,
                };
            }
        }
    }

    // Event-driven: "When <event>, the <system> shall <action>"
    if let Some(rest) = strip_prefix_ci(trimmed, "when ") {
        if let Some((event, system, action)) = parse_event_system_action(rest) {
            return EarsPattern::EventDriven {
                event,
                system,
                action,
            };
        }
    }

    // Unwanted behavior: "If <condition>, then the <system> shall <action>"
    if let Some(rest) = strip_prefix_ci(trimmed, "if ") {
        if let Some(comma_pos) = rest.find(',') {
            let condition = rest[..comma_pos].trim().to_string();
            let after_comma = rest[comma_pos + 1..].trim();
            let after_then = strip_prefix_ci(after_comma, "then ").unwrap_or(after_comma);
            if let Some((system, action)) = parse_system_shall_action(after_then) {
                return EarsPattern::UnwantedBehavior {
                    condition,
                    system,
                    action,
                };
            }
        }
    }

    // Optional: "Where <feature>, the <system> shall <action>"
    if let Some(rest) = strip_prefix_ci(trimmed, "where ") {
        if let Some(comma_pos) = rest.find(',') {
            let feature = rest[..comma_pos].trim().to_string();
            let after_comma = rest[comma_pos + 1..].trim();
            if let Some((system, action)) = parse_system_shall_action(after_comma) {
                return EarsPattern::Optional {
                    feature,
                    system,
                    action,
                };
            }
        }
    }

    // Ubiquitous: "The <system> shall <action>"
    if let Some((system, action)) = parse_system_shall_action(trimmed) {
        return EarsPattern::Ubiquitous { system, action };
    }

    // Freeform fallback
    EarsPattern::Freeform {
        text: text.to_string(),
    }
}

/// Case-insensitive prefix strip.
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

/// Parse "the <system> shall <action>" from a string.
fn parse_system_shall_action(s: &str) -> Option<(String, String)> {
    let rest = strip_prefix_ci(s, "the ")?;
    let shall_pos = rest
        .to_ascii_lowercase()
        .find(" shall ")?;
    let system = rest[..shall_pos].trim().to_string();
    let action = rest[shall_pos + 7..].trim().to_string();
    if system.is_empty() || action.is_empty() {
        return None;
    }
    Some((system, action))
}

/// Parse "<event>, the <system> shall <action>" from a string after "when ".
fn parse_event_system_action(s: &str) -> Option<(String, String, String)> {
    let comma_pos = s.find(',')?;
    let event = s[..comma_pos].trim().to_string();
    let after_comma = s[comma_pos + 1..].trim();
    let (system, action) = parse_system_shall_action(after_comma)?;
    Some((event, system, action))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ubiquitous() {
        let p = parse_ears_pattern("The system shall log all actions.");
        assert_eq!(
            p,
            EarsPattern::Ubiquitous {
                system: "system".to_string(),
                action: "log all actions".to_string(),
            }
        );
    }

    #[test]
    fn test_event_driven() {
        let p = parse_ears_pattern("When a user logs in, the auth service shall issue a token.");
        assert_eq!(
            p,
            EarsPattern::EventDriven {
                event: "a user logs in".to_string(),
                system: "auth service".to_string(),
                action: "issue a token".to_string(),
            }
        );
    }

    #[test]
    fn test_state_driven() {
        let p = parse_ears_pattern(
            "While the agent is in autonomous mode, the scheduler shall check heartbeat.",
        );
        assert_eq!(
            p,
            EarsPattern::StateDriven {
                state: "the agent is in autonomous mode".to_string(),
                system: "scheduler".to_string(),
                action: "check heartbeat".to_string(),
            }
        );
    }

    #[test]
    fn test_unwanted_behavior() {
        let p = parse_ears_pattern(
            "If the violation score exceeds threshold, then the kernel shall downgrade the agent.",
        );
        assert_eq!(
            p,
            EarsPattern::UnwantedBehavior {
                condition: "the violation score exceeds threshold".to_string(),
                system: "kernel".to_string(),
                action: "downgrade the agent".to_string(),
            }
        );
    }

    #[test]
    fn test_optional() {
        let p =
            parse_ears_pattern("Where WASM is enabled, the runtime shall sandbox tool execution.");
        assert_eq!(
            p,
            EarsPattern::Optional {
                feature: "WASM is enabled".to_string(),
                system: "runtime".to_string(),
                action: "sandbox tool execution".to_string(),
            }
        );
    }

    #[test]
    fn test_complex() {
        let p = parse_ears_pattern(
            "While in production, when a circuit breaker trips, the kernel shall record a violation.",
        );
        assert_eq!(
            p,
            EarsPattern::Complex {
                state: "in production".to_string(),
                event: "a circuit breaker trips".to_string(),
                system: "kernel".to_string(),
                action: "record a violation".to_string(),
            }
        );
    }

    #[test]
    fn test_freeform_fallback() {
        let p = parse_ears_pattern("Just do the thing.");
        match p {
            EarsPattern::Freeform { text } => assert_eq!(text, "Just do the thing."),
            other => panic!("Expected Freeform, got {:?}", other),
        }
    }

    #[test]
    fn test_case_insensitive() {
        let p = parse_ears_pattern("WHEN something happens, THE system SHALL respond.");
        match p {
            EarsPattern::EventDriven { event, system, action } => {
                assert_eq!(event, "something happens");
                assert_eq!(system, "system");
                assert_eq!(action, "respond");
            }
            other => panic!("Expected EventDriven, got {:?}", other),
        }
    }

    #[test]
    fn test_spec_to_markdown() {
        let mut spec = EarsSpec::new("Agent Safety");
        spec.add("REQ-001", "The kernel shall enforce budget limits.");
        spec.add("REQ-002", "When a tool fails, the runtime shall log the error.");
        spec.requirements[0].status = RequirementStatus::Verified;
        spec.requirements[1].priority = RequirementPriority::Critical;

        let md = spec.to_prompt_markdown();
        assert!(md.contains("Agent Safety"));
        assert!(md.contains("[x] **REQ-001**"));
        assert!(md.contains("[ ] **REQ-002** (CRITICAL)"));
    }

    #[test]
    fn test_count_by_status() {
        let mut spec = EarsSpec::new("test");
        spec.add("R1", "The system shall do A.");
        spec.add("R2", "The system shall do B.");
        spec.add("R3", "The system shall do C.");
        spec.requirements[0].status = RequirementStatus::Verified;
        spec.requirements[1].status = RequirementStatus::Verified;

        assert_eq!(spec.count_by_status(RequirementStatus::Verified), 2);
        assert_eq!(spec.count_by_status(RequirementStatus::Pending), 1);
    }

    #[test]
    fn test_serde_roundtrip() {
        let mut spec = EarsSpec::new("test");
        spec.add("R1", "When X happens, the system shall do Y.");
        let json = serde_json::to_string(&spec).unwrap();
        let parsed: EarsSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.requirements[0].id, "R1");
        assert_eq!(parsed.requirements.len(), 1);
    }
}
