//! Property-Based Testing (PBT) for Spec-Driven Development.
//!
//! Translates EARS requirements into runtime-checkable invariants that can be
//! validated against arbitrary agent responses. Unlike unit tests that check
//! specific outputs, PBT defines properties that must hold for ALL valid inputs.
//!
//! The flow:
//! 1. EARS requirements are parsed into structured patterns
//! 2. Each requirement generates one or more `PropertyInvariant`s
//! 3. Invariants are checked against agent responses using pattern matching
//! 4. Failures identify which specific requirement was violated

use openfang_types::ears::{EarsPattern, EarsRequirement, EarsSpec};
use serde::{Deserialize, Serialize};

/// A property invariant derived from an EARS requirement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyInvariant {
    /// Originating requirement ID (e.g., "REQ-001").
    pub requirement_id: String,
    /// Human-readable invariant description.
    pub description: String,
    /// The check to perform.
    pub check: InvariantCheck,
    /// Whether violation should block or just warn.
    pub severity: InvariantSeverity,
}

/// Types of invariant checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InvariantCheck {
    /// Response must contain this substring (case-insensitive).
    ResponseContains { pattern: String },
    /// Response must NOT contain this substring.
    ResponseExcludes { pattern: String },
    /// Response length must be within bounds.
    ResponseLength { min: Option<usize>, max: Option<usize> },
    /// When a trigger phrase appears in the input, the response must contain the expected pattern.
    ConditionalContains { trigger: String, expected: String },
    /// When a trigger phrase appears in the input, the response must NOT contain the pattern.
    ConditionalExcludes { trigger: String, forbidden: String },
    /// The response must match a custom regex pattern.
    RegexMatch { pattern: String },
    /// A composite check: all sub-checks must pass.
    All { checks: Vec<InvariantCheck> },
    /// A composite check: at least one sub-check must pass.
    Any { checks: Vec<InvariantCheck> },
}

/// Severity of invariant violations.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum InvariantSeverity {
    /// Must pass — blocks the response.
    Critical,
    /// Should pass — logs a warning.
    Warning,
    /// Nice to have — informational only.
    Info,
}

/// Result of checking a single invariant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvariantResult {
    pub requirement_id: String,
    pub description: String,
    pub passed: bool,
    pub severity: InvariantSeverity,
    pub details: Option<String>,
}

/// Result of checking all invariants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PbtReport {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub critical_failures: usize,
    pub results: Vec<InvariantResult>,
}

impl PbtReport {
    /// Whether all critical invariants passed.
    pub fn critical_pass(&self) -> bool {
        self.critical_failures == 0
    }
}

/// Generate property invariants from an EARS specification.
pub fn generate_invariants(spec: &EarsSpec) -> Vec<PropertyInvariant> {
    let mut invariants = Vec::new();

    for req in &spec.requirements {
        invariants.extend(requirement_to_invariants(req));
    }

    invariants
}

/// Convert a single EARS requirement into one or more property invariants.
fn requirement_to_invariants(req: &EarsRequirement) -> Vec<PropertyInvariant> {
    let mut invariants = Vec::new();
    let severity = match req.priority {
        openfang_types::ears::RequirementPriority::Critical => InvariantSeverity::Critical,
        openfang_types::ears::RequirementPriority::High => InvariantSeverity::Warning,
        openfang_types::ears::RequirementPriority::Medium | openfang_types::ears::RequirementPriority::Low => InvariantSeverity::Info,
    };

    match &req.pattern {
        EarsPattern::Ubiquitous { action, .. } => {
            // Ubiquitous requirements ALWAYS apply — extract keywords from action
            let keywords = extract_action_keywords(action);
            if !keywords.is_empty() {
                invariants.push(PropertyInvariant {
                    requirement_id: req.id.clone(),
                    description: format!("Ubiquitous: {}", req.text),
                    check: InvariantCheck::ResponseContains {
                        pattern: keywords[0].clone(),
                    },
                    severity,
                });
            }
        }
        EarsPattern::EventDriven { event, action, .. } => {
            // When <event>, the system shall <action>
            let trigger = extract_trigger(event);
            let expected = extract_action_keywords(action);
            if !expected.is_empty() {
                invariants.push(PropertyInvariant {
                    requirement_id: req.id.clone(),
                    description: format!("Event-driven: when '{}' -> '{}'", event, action),
                    check: InvariantCheck::ConditionalContains {
                        trigger,
                        expected: expected[0].clone(),
                    },
                    severity,
                });
            }
        }
        EarsPattern::StateDriven { state, action, .. } => {
            // While <state>, the system shall <action>
            let trigger = extract_trigger(state);
            let expected = extract_action_keywords(action);
            if !expected.is_empty() {
                invariants.push(PropertyInvariant {
                    requirement_id: req.id.clone(),
                    description: format!("State-driven: while '{}' -> '{}'", state, action),
                    check: InvariantCheck::ConditionalContains {
                        trigger,
                        expected: expected[0].clone(),
                    },
                    severity,
                });
            }
        }
        EarsPattern::UnwantedBehavior {
            condition, action, ..
        } => {
            // If <condition>, then the system shall <action> (usually prevent something)
            let trigger = extract_trigger(condition);
            let action_kw = extract_action_keywords(action);
            // For unwanted behavior, look for negation patterns
            if action.to_lowercase().contains("not")
                || action.to_lowercase().contains("reject")
                || action.to_lowercase().contains("block")
                || action.to_lowercase().contains("prevent")
                || action.to_lowercase().contains("deny")
            {
                // The action is about preventing — check that forbidden content is excluded
                if !action_kw.is_empty() {
                    invariants.push(PropertyInvariant {
                        requirement_id: req.id.clone(),
                        description: format!("Unwanted: if '{}' then prevent", condition),
                        check: InvariantCheck::ConditionalExcludes {
                            trigger,
                            forbidden: extract_forbidden(action),
                        },
                        severity,
                    });
                }
            } else if !action_kw.is_empty() {
                invariants.push(PropertyInvariant {
                    requirement_id: req.id.clone(),
                    description: format!("Unwanted: if '{}' then '{}'", condition, action),
                    check: InvariantCheck::ConditionalContains {
                        trigger,
                        expected: action_kw[0].clone(),
                    },
                    severity,
                });
            }
        }
        EarsPattern::Optional { feature, action, .. } => {
            let trigger = extract_trigger(feature);
            let expected = extract_action_keywords(action);
            if !expected.is_empty() {
                invariants.push(PropertyInvariant {
                    requirement_id: req.id.clone(),
                    description: format!("Optional: where '{}' -> '{}'", feature, action),
                    check: InvariantCheck::ConditionalContains {
                        trigger,
                        expected: expected[0].clone(),
                    },
                    severity: InvariantSeverity::Info, // Optional features are never critical
                });
            }
        }
        EarsPattern::Complex {
            state,
            event,
            action,
            ..
        } => {
            // Both state AND event must appear for the invariant to apply
            let combined_trigger = format!("{} {}", state, event);
            let expected = extract_action_keywords(action);
            if !expected.is_empty() {
                invariants.push(PropertyInvariant {
                    requirement_id: req.id.clone(),
                    description: format!(
                        "Complex: while '{}' when '{}' -> '{}'",
                        state, event, action
                    ),
                    check: InvariantCheck::ConditionalContains {
                        trigger: combined_trigger,
                        expected: expected[0].clone(),
                    },
                    severity,
                });
            }
        }
        EarsPattern::Freeform { text } => {
            // Can't auto-generate invariants from freeform text
            // but we can check for obvious patterns
            if text.to_lowercase().contains("must not")
                || text.to_lowercase().contains("shall not")
            {
                if let Some(forbidden) = extract_forbidden_from_text(text) {
                    invariants.push(PropertyInvariant {
                        requirement_id: req.id.clone(),
                        description: format!("Freeform exclusion: {}", &text[..text.len().min(80)]),
                        check: InvariantCheck::ResponseExcludes {
                            pattern: forbidden,
                        },
                        severity,
                    });
                }
            }
        }
    }

    invariants
}

/// Check all invariants against an input/response pair.
pub fn check_invariants(
    invariants: &[PropertyInvariant],
    user_input: &str,
    response: &str,
) -> PbtReport {
    let mut results = Vec::new();

    for inv in invariants {
        let (passed, details) = eval_check(&inv.check, user_input, response);
        results.push(InvariantResult {
            requirement_id: inv.requirement_id.clone(),
            description: inv.description.clone(),
            passed,
            severity: inv.severity,
            details,
        });
    }

    let total = results.len();
    let passed = results.iter().filter(|r| r.passed).count();
    let failed = total - passed;
    let critical_failures = results
        .iter()
        .filter(|r| !r.passed && r.severity == InvariantSeverity::Critical)
        .count();

    PbtReport {
        total,
        passed,
        failed,
        critical_failures,
        results,
    }
}

/// Evaluate a single invariant check.
fn eval_check(check: &InvariantCheck, input: &str, response: &str) -> (bool, Option<String>) {
    let resp_lower = response.to_lowercase();
    let input_lower = input.to_lowercase();

    match check {
        InvariantCheck::ResponseContains { pattern } => {
            let found = resp_lower.contains(&pattern.to_lowercase());
            (
                found,
                if found {
                    None
                } else {
                    Some(format!("Response missing expected pattern: '{}'", pattern))
                },
            )
        }
        InvariantCheck::ResponseExcludes { pattern } => {
            let found = resp_lower.contains(&pattern.to_lowercase());
            (
                !found,
                if found {
                    Some(format!("Response contains forbidden pattern: '{}'", pattern))
                } else {
                    None
                },
            )
        }
        InvariantCheck::ResponseLength { min, max } => {
            let len = response.len();
            let pass_min = min.is_none_or(|m| len >= m);
            let pass_max = max.is_none_or(|m| len <= m);
            let passed = pass_min && pass_max;
            (
                passed,
                if passed {
                    None
                } else {
                    Some(format!(
                        "Response length {} outside bounds [{}, {}]",
                        len,
                        min.unwrap_or(0),
                        max.unwrap_or(usize::MAX)
                    ))
                },
            )
        }
        InvariantCheck::ConditionalContains { trigger, expected } => {
            // Only check if the trigger appears in the input
            if !input_lower.contains(&trigger.to_lowercase()) {
                return (true, Some("Trigger not present — invariant vacuously true".into()));
            }
            let found = resp_lower.contains(&expected.to_lowercase());
            (
                found,
                if found {
                    None
                } else {
                    Some(format!(
                        "Trigger '{}' present but response missing '{}'",
                        trigger, expected
                    ))
                },
            )
        }
        InvariantCheck::ConditionalExcludes { trigger, forbidden } => {
            if !input_lower.contains(&trigger.to_lowercase()) {
                return (true, Some("Trigger not present — invariant vacuously true".into()));
            }
            let found = resp_lower.contains(&forbidden.to_lowercase());
            (
                !found,
                if found {
                    Some(format!(
                        "Trigger '{}' present and response contains forbidden '{}'",
                        trigger, forbidden
                    ))
                } else {
                    None
                },
            )
        }
        InvariantCheck::RegexMatch { pattern } => {
            match regex_lite::Regex::new(pattern) {
                Ok(re) => {
                    let matched = re.is_match(response);
                    (
                        matched,
                        if matched {
                            None
                        } else {
                            Some(format!("Response did not match regex: {}", pattern))
                        },
                    )
                }
                Err(e) => (false, Some(format!("Invalid regex '{}': {}", pattern, e))),
            }
        }
        InvariantCheck::All { checks } => {
            let mut all_pass = true;
            let mut details = Vec::new();
            for c in checks {
                let (passed, detail) = eval_check(c, input, response);
                if !passed {
                    all_pass = false;
                    if let Some(d) = detail {
                        details.push(d);
                    }
                }
            }
            (
                all_pass,
                if details.is_empty() {
                    None
                } else {
                    Some(details.join("; "))
                },
            )
        }
        InvariantCheck::Any { checks } => {
            let mut any_pass = false;
            for c in checks {
                let (passed, _) = eval_check(c, input, response);
                if passed {
                    any_pass = true;
                    break;
                }
            }
            (
                any_pass,
                if any_pass {
                    None
                } else {
                    Some("None of the alternative checks passed".into())
                },
            )
        }
    }
}

/// Extract meaningful keywords from an EARS action clause.
fn extract_action_keywords(action: &str) -> Vec<String> {
    let stop_words = [
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "being",
        "have", "has", "had", "do", "does", "did", "will", "would", "could",
        "should", "may", "might", "shall", "can", "must", "to", "of", "in",
        "for", "on", "with", "at", "by", "from", "as", "into", "through",
        "during", "before", "after", "above", "below", "between", "out",
        "off", "over", "under", "again", "further", "then", "once", "and",
        "but", "or", "nor", "not", "no", "all", "each", "every", "both",
        "few", "more", "most", "other", "some", "such", "only", "own",
        "same", "so", "than", "too", "very", "just", "also", "it", "its",
        "that", "this", "these", "those", "there",
    ];

    action
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        .filter(|w| w.len() > 2 && !stop_words.contains(&w.as_str()))
        .collect()
}

/// Extract a trigger phrase for conditional checks.
fn extract_trigger(text: &str) -> String {
    // Use the first 3-4 significant words as the trigger
    let keywords = extract_action_keywords(text);
    keywords.into_iter().take(3).collect::<Vec<_>>().join(" ")
}

/// Extract what should be forbidden from a negation action.
fn extract_forbidden(action: &str) -> String {
    let lower = action.to_lowercase();
    // Find what comes after negation words
    for neg in ["not ", "reject ", "block ", "prevent ", "deny "] {
        if let Some(pos) = lower.find(neg) {
            let rest = &action[pos + neg.len()..];
            let keywords = extract_action_keywords(rest);
            if !keywords.is_empty() {
                return keywords[0].clone();
            }
        }
    }
    // Fallback: use first significant keyword
    extract_action_keywords(action)
        .first()
        .cloned()
        .unwrap_or_default()
}

/// Try to extract a forbidden term from freeform text with "must not"/"shall not".
fn extract_forbidden_from_text(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    for phrase in ["must not ", "shall not "] {
        if let Some(pos) = lower.find(phrase) {
            let rest = &text[pos + phrase.len()..];
            let keywords = extract_action_keywords(rest);
            if !keywords.is_empty() {
                return Some(keywords[0].clone());
            }
        }
    }
    None
}

/// Generate a PBT report as a concise markdown summary.
pub fn report_to_markdown(report: &PbtReport) -> String {
    let mut out = format!(
        "## PBT Report: {}/{} passed",
        report.passed, report.total
    );
    if report.critical_failures > 0 {
        out.push_str(&format!(" ({} CRITICAL FAILURES)", report.critical_failures));
    }
    out.push('\n');

    for r in &report.results {
        let icon = if r.passed { "PASS" } else { "FAIL" };
        let sev = match r.severity {
            InvariantSeverity::Critical => "CRIT",
            InvariantSeverity::Warning => "WARN",
            InvariantSeverity::Info => "INFO",
        };
        out.push_str(&format!(
            "- [{}][{}] {}: {}",
            icon, sev, r.requirement_id, r.description
        ));
        if let Some(ref d) = r.details {
            out.push_str(&format!(" — {}", d));
        }
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfang_types::ears::*;

    fn make_req(id: &str, pattern: EarsPattern) -> EarsRequirement {
        EarsRequirement {
            id: id.into(),
            text: format!("Test requirement {}", id),
            pattern,
            priority: RequirementPriority::Critical,
            status: RequirementStatus::Pending,
            tags: vec![],
        }
    }

    #[test]
    fn test_ubiquitous_generates_invariant() {
        let req = make_req(
            "REQ-001",
            EarsPattern::Ubiquitous {
                system: "agent".into(),
                action: "respond with valid JSON".into(),
            },
        );
        let invs = requirement_to_invariants(&req);
        assert!(!invs.is_empty());
        assert_eq!(invs[0].requirement_id, "REQ-001");
    }

    #[test]
    fn test_event_driven_conditional() {
        let req = make_req(
            "REQ-002",
            EarsPattern::EventDriven {
                event: "user requests help".into(),
                system: "agent".into(),
                action: "display documentation link".into(),
            },
        );
        let invs = requirement_to_invariants(&req);
        assert!(!invs.is_empty());
        // Check that the invariant is conditional
        match &invs[0].check {
            InvariantCheck::ConditionalContains { trigger, .. } => {
                assert!(!trigger.is_empty());
            }
            _ => panic!("Expected ConditionalContains"),
        }
    }

    #[test]
    fn test_unwanted_behavior_exclusion() {
        let req = make_req(
            "REQ-003",
            EarsPattern::UnwantedBehavior {
                condition: "malicious input detected".into(),
                system: "agent".into(),
                action: "reject the request and block execution".into(),
            },
        );
        let invs = requirement_to_invariants(&req);
        assert!(!invs.is_empty());
        match &invs[0].check {
            InvariantCheck::ConditionalExcludes { .. } => {}
            _ => panic!("Expected ConditionalExcludes for unwanted behavior with 'reject'"),
        }
    }

    #[test]
    fn test_check_response_contains() {
        let inv = PropertyInvariant {
            requirement_id: "T-1".into(),
            description: "test".into(),
            check: InvariantCheck::ResponseContains {
                pattern: "hello".into(),
            },
            severity: InvariantSeverity::Critical,
        };
        let report = check_invariants(&[inv], "greet me", "Hello World!");
        assert!(report.critical_pass());
        assert_eq!(report.passed, 1);
    }

    #[test]
    fn test_check_response_contains_fail() {
        let inv = PropertyInvariant {
            requirement_id: "T-2".into(),
            description: "test".into(),
            check: InvariantCheck::ResponseContains {
                pattern: "goodbye".into(),
            },
            severity: InvariantSeverity::Critical,
        };
        let report = check_invariants(&[inv], "greet me", "Hello World!");
        assert!(!report.critical_pass());
        assert_eq!(report.failed, 1);
    }

    #[test]
    fn test_conditional_vacuously_true() {
        let inv = PropertyInvariant {
            requirement_id: "T-3".into(),
            description: "test".into(),
            check: InvariantCheck::ConditionalContains {
                trigger: "error".into(),
                expected: "sorry".into(),
            },
            severity: InvariantSeverity::Critical,
        };
        // Trigger not present — should pass vacuously
        let report = check_invariants(&[inv], "how are you", "I'm fine!");
        assert!(report.critical_pass());
    }

    #[test]
    fn test_conditional_triggered_pass() {
        let inv = PropertyInvariant {
            requirement_id: "T-4".into(),
            description: "test".into(),
            check: InvariantCheck::ConditionalContains {
                trigger: "error".into(),
                expected: "sorry".into(),
            },
            severity: InvariantSeverity::Critical,
        };
        let report = check_invariants(&[inv], "there was an error", "I'm sorry about that!");
        assert!(report.critical_pass());
    }

    #[test]
    fn test_conditional_triggered_fail() {
        let inv = PropertyInvariant {
            requirement_id: "T-5".into(),
            description: "test".into(),
            check: InvariantCheck::ConditionalContains {
                trigger: "error".into(),
                expected: "sorry".into(),
            },
            severity: InvariantSeverity::Critical,
        };
        let report = check_invariants(&[inv], "there was an error", "Everything is fine!");
        assert!(!report.critical_pass());
    }

    #[test]
    fn test_response_length() {
        let inv = PropertyInvariant {
            requirement_id: "T-6".into(),
            description: "test".into(),
            check: InvariantCheck::ResponseLength {
                min: Some(5),
                max: Some(100),
            },
            severity: InvariantSeverity::Warning,
        };
        let report = check_invariants(std::slice::from_ref(&inv), "", "Hi");
        assert_eq!(report.failed, 1);

        let report = check_invariants(std::slice::from_ref(&inv), "", "Hello World!");
        assert_eq!(report.passed, 1);
    }

    #[test]
    fn test_composite_all() {
        let inv = PropertyInvariant {
            requirement_id: "T-7".into(),
            description: "test".into(),
            check: InvariantCheck::All {
                checks: vec![
                    InvariantCheck::ResponseContains {
                        pattern: "hello".into(),
                    },
                    InvariantCheck::ResponseExcludes {
                        pattern: "goodbye".into(),
                    },
                ],
            },
            severity: InvariantSeverity::Critical,
        };
        let report = check_invariants(&[inv], "", "Hello there!");
        assert!(report.critical_pass());
    }

    #[test]
    fn test_composite_any() {
        let inv = PropertyInvariant {
            requirement_id: "T-8".into(),
            description: "test".into(),
            check: InvariantCheck::Any {
                checks: vec![
                    InvariantCheck::ResponseContains {
                        pattern: "hello".into(),
                    },
                    InvariantCheck::ResponseContains {
                        pattern: "hi".into(),
                    },
                ],
            },
            severity: InvariantSeverity::Critical,
        };
        let report = check_invariants(&[inv], "", "Hi there!");
        assert!(report.critical_pass());
    }

    #[test]
    fn test_generate_invariants_from_spec() {
        let mut spec = EarsSpec::new("test-spec");
        {
            let req = spec.add("REQ-001", "The agent shall respond with a greeting");
            req.priority = RequirementPriority::Critical;
        }
        {
            let req = spec.add(
                "REQ-002",
                "When an error occurs, the agent shall apologize to the user",
            );
            req.priority = RequirementPriority::High;
        }

        let invariants = generate_invariants(&spec);
        assert_eq!(invariants.len(), 2);
        assert_eq!(invariants[0].severity, InvariantSeverity::Critical);
        assert_eq!(invariants[1].severity, InvariantSeverity::Warning);
    }

    #[test]
    fn test_report_to_markdown() {
        let report = PbtReport {
            total: 3,
            passed: 2,
            failed: 1,
            critical_failures: 1,
            results: vec![
                InvariantResult {
                    requirement_id: "R-1".into(),
                    description: "test pass".into(),
                    passed: true,
                    severity: InvariantSeverity::Critical,
                    details: None,
                },
                InvariantResult {
                    requirement_id: "R-2".into(),
                    description: "test fail".into(),
                    passed: false,
                    severity: InvariantSeverity::Critical,
                    details: Some("missing pattern".into()),
                },
            ],
        };
        let md = report_to_markdown(&report);
        assert!(md.contains("2/3 passed"));
        assert!(md.contains("CRITICAL FAILURES"));
        assert!(md.contains("[PASS]"));
        assert!(md.contains("[FAIL]"));
    }

    #[test]
    fn test_extract_action_keywords() {
        let kw = extract_action_keywords("respond with valid JSON output");
        assert!(kw.contains(&"respond".to_string()));
        assert!(kw.contains(&"valid".to_string()));
        assert!(kw.contains(&"json".to_string()));
        // Stop words should be filtered
        assert!(!kw.contains(&"with".to_string()));
    }

    #[test]
    fn test_freeform_shall_not() {
        let req = make_req(
            "REQ-F1",
            EarsPattern::Freeform {
                text: "The system shall not expose credentials in output".into(),
            },
        );
        let invs = requirement_to_invariants(&req);
        assert!(!invs.is_empty());
        match &invs[0].check {
            InvariantCheck::ResponseExcludes { pattern } => {
                assert!(!pattern.is_empty());
            }
            _ => panic!("Expected ResponseExcludes for 'shall not'"),
        }
    }

    #[test]
    fn test_optional_always_info() {
        let req = make_req(
            "REQ-O1",
            EarsPattern::Optional {
                feature: "dark mode enabled".into(),
                system: "UI".into(),
                action: "use dark color scheme".into(),
            },
        );
        let invs = requirement_to_invariants(&req);
        assert!(!invs.is_empty());
        assert_eq!(invs[0].severity, InvariantSeverity::Info);
    }

    #[test]
    fn test_regex_match() {
        let inv = PropertyInvariant {
            requirement_id: "T-R1".into(),
            description: "test".into(),
            check: InvariantCheck::RegexMatch {
                pattern: r"\d{3}-\d{4}".into(),
            },
            severity: InvariantSeverity::Warning,
        };
        let report = check_invariants(&[inv], "", "Call 555-1234 for info");
        assert_eq!(report.passed, 1);
    }

    #[test]
    fn test_serde_roundtrip() {
        let inv = PropertyInvariant {
            requirement_id: "T-S1".into(),
            description: "serde test".into(),
            check: InvariantCheck::ConditionalContains {
                trigger: "error".into(),
                expected: "sorry".into(),
            },
            severity: InvariantSeverity::Critical,
        };
        let json = serde_json::to_string(&inv).unwrap();
        let parsed: PropertyInvariant = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.requirement_id, "T-S1");
    }
}
