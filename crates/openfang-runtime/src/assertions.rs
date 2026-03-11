//! Runtime assertions — neurosymbolic validation for agent behavior.
//!
//! Agents can declare behavioral invariants that are checked at runtime.
//! When an assertion fails, the system records a violation and can trigger
//! corrective action (retry, downgrade, or human review).
//!
//! This bridges the gap between symbolic (rule-based) and neural (LLM-based)
//! verification — the blueprint's "neurosymbolic validation" factor.

use serde::{Deserialize, Serialize};

/// A runtime assertion about agent behavior.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeAssertion {
    /// Unique assertion name.
    pub name: String,
    /// The condition being checked.
    pub condition: AssertionCondition,
    /// What to do when the assertion fails.
    #[serde(default)]
    pub on_fail: FailAction,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
}

/// Conditions that can be checked at runtime.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssertionCondition {
    /// Response must not exceed a token count.
    MaxResponseTokens { limit: usize },
    /// Response must contain a specific string or pattern.
    ResponseContains { pattern: String, case_sensitive: bool },
    /// Response must NOT contain a specific string or pattern.
    ResponseExcludes { pattern: String, case_sensitive: bool },
    /// Tool calls must not exceed a count in one turn.
    MaxToolCalls { limit: usize },
    /// Total cost for this message must not exceed amount.
    MaxCostUsd { limit: f64 },
    /// Response must be shorter than N characters.
    MaxResponseLength { limit: usize },
    /// No tool calls to the specified tool names.
    ForbiddenTools { tools: Vec<String> },
    /// Response must mention at least one of these keywords.
    RequiredKeywords { keywords: Vec<String> },
}

/// Action to take when an assertion fails.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FailAction {
    /// Log a warning but allow the response through.
    #[default]
    Warn,
    /// Record a violation (counts toward auto-downgrade).
    Violate,
    /// Block the response and return an error to the user.
    Block,
    /// Request human review via the approval system.
    Review,
}

/// Result of checking a set of assertions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssertionCheckResult {
    /// All assertions passed.
    pub all_passed: bool,
    /// Individual results.
    pub results: Vec<SingleAssertionResult>,
}

/// Result of a single assertion check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SingleAssertionResult {
    /// Assertion name.
    pub name: String,
    /// Whether it passed.
    pub passed: bool,
    /// What action to take if failed.
    pub fail_action: FailAction,
    /// Reason for failure (empty if passed).
    pub reason: String,
}

/// Check a response against a set of runtime assertions.
pub fn check_assertions(
    assertions: &[RuntimeAssertion],
    response: &str,
    tool_call_count: usize,
    cost_usd: f64,
) -> AssertionCheckResult {
    let mut results = Vec::with_capacity(assertions.len());
    let mut all_passed = true;

    for assertion in assertions {
        let (passed, reason) =
            evaluate_condition(&assertion.condition, response, tool_call_count, cost_usd);
        if !passed {
            all_passed = false;
        }
        results.push(SingleAssertionResult {
            name: assertion.name.clone(),
            passed,
            fail_action: assertion.on_fail,
            reason,
        });
    }

    AssertionCheckResult {
        all_passed,
        results,
    }
}

/// Evaluate a single assertion condition.
fn evaluate_condition(
    condition: &AssertionCondition,
    response: &str,
    tool_call_count: usize,
    cost_usd: f64,
) -> (bool, String) {
    match condition {
        AssertionCondition::MaxResponseTokens { limit } => {
            let estimated = response.len() / 4;
            if estimated > *limit {
                (
                    false,
                    format!("Response ~{estimated} tokens exceeds limit of {limit}"),
                )
            } else {
                (true, String::new())
            }
        }

        AssertionCondition::ResponseContains {
            pattern,
            case_sensitive,
        } => {
            let found = if *case_sensitive {
                response.contains(pattern.as_str())
            } else {
                response.to_lowercase().contains(&pattern.to_lowercase())
            };
            if found {
                (true, String::new())
            } else {
                (false, format!("Response does not contain '{pattern}'"))
            }
        }

        AssertionCondition::ResponseExcludes {
            pattern,
            case_sensitive,
        } => {
            let found = if *case_sensitive {
                response.contains(pattern.as_str())
            } else {
                response.to_lowercase().contains(&pattern.to_lowercase())
            };
            if found {
                (false, format!("Response contains forbidden pattern '{pattern}'"))
            } else {
                (true, String::new())
            }
        }

        AssertionCondition::MaxToolCalls { limit } => {
            if tool_call_count > *limit {
                (
                    false,
                    format!("{tool_call_count} tool calls exceeds limit of {limit}"),
                )
            } else {
                (true, String::new())
            }
        }

        AssertionCondition::MaxCostUsd { limit } => {
            if cost_usd > *limit {
                (
                    false,
                    format!("Cost ${cost_usd:.4} exceeds limit of ${limit:.4}"),
                )
            } else {
                (true, String::new())
            }
        }

        AssertionCondition::MaxResponseLength { limit } => {
            if response.len() > *limit {
                (
                    false,
                    format!(
                        "Response length {} exceeds limit of {limit}",
                        response.len()
                    ),
                )
            } else {
                (true, String::new())
            }
        }

        AssertionCondition::ForbiddenTools { tools } => {
            // This check requires tool names — we can only check response text for tool mentions
            let response_lower = response.to_lowercase();
            for tool in tools {
                if response_lower.contains(&tool.to_lowercase()) {
                    return (
                        false,
                        format!("Response mentions forbidden tool '{tool}'"),
                    );
                }
            }
            (true, String::new())
        }

        AssertionCondition::RequiredKeywords { keywords } => {
            let response_lower = response.to_lowercase();
            for kw in keywords {
                if response_lower.contains(&kw.to_lowercase()) {
                    return (true, String::new());
                }
            }
            (
                false,
                format!(
                    "Response does not contain any of: {}",
                    keywords.join(", ")
                ),
            )
        }
    }
}

/// Parse assertions from a JSON assertions file.
///
/// Format:
/// ```json
/// { "assertions": [
///   { "name": "max-tokens", "condition": { "type": "max_response_tokens", "limit": 1000 }, "on_fail": "warn" }
/// ]}
/// ```
pub fn parse_assertions_json(content: &str) -> Result<Vec<RuntimeAssertion>, String> {
    #[derive(Deserialize)]
    struct AssertionsFile {
        #[serde(default)]
        assertions: Vec<RuntimeAssertion>,
    }

    serde_json::from_str::<AssertionsFile>(content)
        .map(|f| f.assertions)
        .map_err(|e| format!("Failed to parse assertions: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_assertions() -> Vec<RuntimeAssertion> {
        vec![
            RuntimeAssertion {
                name: "max-tokens".to_string(),
                condition: AssertionCondition::MaxResponseTokens { limit: 500 },
                on_fail: FailAction::Warn,
                description: String::new(),
            },
            RuntimeAssertion {
                name: "no-profanity".to_string(),
                condition: AssertionCondition::ResponseExcludes {
                    pattern: "badword".to_string(),
                    case_sensitive: false,
                },
                on_fail: FailAction::Violate,
                description: String::new(),
            },
            RuntimeAssertion {
                name: "max-tools".to_string(),
                condition: AssertionCondition::MaxToolCalls { limit: 5 },
                on_fail: FailAction::Block,
                description: String::new(),
            },
        ]
    }

    #[test]
    fn test_all_pass() {
        let assertions = sample_assertions();
        let result = check_assertions(&assertions, "Hello world", 2, 0.01);
        assert!(result.all_passed);
        assert_eq!(result.results.len(), 3);
        assert!(result.results.iter().all(|r| r.passed));
    }

    #[test]
    fn test_token_limit_exceeded() {
        let assertions = vec![RuntimeAssertion {
            name: "short".to_string(),
            condition: AssertionCondition::MaxResponseTokens { limit: 10 },
            on_fail: FailAction::Warn,
            description: String::new(),
        }];
        let long = "x".repeat(200);
        let result = check_assertions(&assertions, &long, 0, 0.0);
        assert!(!result.all_passed);
        assert!(!result.results[0].passed);
        assert!(result.results[0].reason.contains("exceeds limit"));
    }

    #[test]
    fn test_contains_check() {
        let assertions = vec![RuntimeAssertion {
            name: "must-greet".to_string(),
            condition: AssertionCondition::ResponseContains {
                pattern: "hello".to_string(),
                case_sensitive: false,
            },
            on_fail: FailAction::Warn,
            description: String::new(),
        }];
        let pass = check_assertions(&assertions, "Hello there!", 0, 0.0);
        assert!(pass.all_passed);

        let fail = check_assertions(&assertions, "Goodbye!", 0, 0.0);
        assert!(!fail.all_passed);
    }

    #[test]
    fn test_excludes_check() {
        let assertions = sample_assertions();
        let result = check_assertions(&assertions, "This contains badword here", 2, 0.01);
        assert!(!result.all_passed);
        assert!(!result.results[1].passed);
        assert_eq!(result.results[1].fail_action, FailAction::Violate);
    }

    #[test]
    fn test_tool_call_limit() {
        let assertions = sample_assertions();
        let result = check_assertions(&assertions, "OK", 10, 0.01);
        assert!(!result.all_passed);
        assert!(!result.results[2].passed);
        assert_eq!(result.results[2].fail_action, FailAction::Block);
    }

    #[test]
    fn test_cost_limit() {
        let assertions = vec![RuntimeAssertion {
            name: "cheap".to_string(),
            condition: AssertionCondition::MaxCostUsd { limit: 0.05 },
            on_fail: FailAction::Violate,
            description: String::new(),
        }];
        let pass = check_assertions(&assertions, "OK", 0, 0.01);
        assert!(pass.all_passed);

        let fail = check_assertions(&assertions, "OK", 0, 0.10);
        assert!(!fail.all_passed);
    }

    #[test]
    fn test_required_keywords() {
        let assertions = vec![RuntimeAssertion {
            name: "must-cite".to_string(),
            condition: AssertionCondition::RequiredKeywords {
                keywords: vec!["source".to_string(), "reference".to_string()],
            },
            on_fail: FailAction::Warn,
            description: String::new(),
        }];
        let pass = check_assertions(&assertions, "According to my source...", 0, 0.0);
        assert!(pass.all_passed);

        let fail = check_assertions(&assertions, "I think this is true.", 0, 0.0);
        assert!(!fail.all_passed);
    }

    #[test]
    fn test_response_length() {
        let assertions = vec![RuntimeAssertion {
            name: "short".to_string(),
            condition: AssertionCondition::MaxResponseLength { limit: 50 },
            on_fail: FailAction::Warn,
            description: String::new(),
        }];
        let pass = check_assertions(&assertions, "Short response", 0, 0.0);
        assert!(pass.all_passed);

        let fail = check_assertions(&assertions, &"x".repeat(100), 0, 0.0);
        assert!(!fail.all_passed);
    }

    #[test]
    fn test_forbidden_tools() {
        let assertions = vec![RuntimeAssertion {
            name: "no-shell".to_string(),
            condition: AssertionCondition::ForbiddenTools {
                tools: vec!["shell_exec".to_string()],
            },
            on_fail: FailAction::Block,
            description: String::new(),
        }];
        let pass = check_assertions(&assertions, "I used file_read to check.", 0, 0.0);
        assert!(pass.all_passed);

        let fail = check_assertions(&assertions, "I ran shell_exec to compile.", 0, 0.0);
        assert!(!fail.all_passed);
    }

    #[test]
    fn test_parse_json() {
        let json = r#"{
  "assertions": [
    {
      "name": "max-tokens",
      "description": "Keep responses concise",
      "on_fail": "warn",
      "condition": { "type": "max_response_tokens", "limit": 1000 }
    },
    {
      "name": "no-secrets",
      "on_fail": "block",
      "condition": { "type": "response_excludes", "pattern": "sk-", "case_sensitive": true }
    }
  ]
}"#;
        let assertions = parse_assertions_json(json).unwrap();
        assert_eq!(assertions.len(), 2);
        assert_eq!(assertions[0].name, "max-tokens");
        assert_eq!(assertions[0].on_fail, FailAction::Warn);
        assert_eq!(assertions[1].name, "no-secrets");
        assert_eq!(assertions[1].on_fail, FailAction::Block);
    }

    #[test]
    fn test_serde_roundtrip() {
        let assertion = RuntimeAssertion {
            name: "test".to_string(),
            condition: AssertionCondition::MaxCostUsd { limit: 0.50 },
            on_fail: FailAction::Review,
            description: "Keep costs low".to_string(),
        };
        let json = serde_json::to_string(&assertion).unwrap();
        let parsed: RuntimeAssertion = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "test");
        assert_eq!(parsed.on_fail, FailAction::Review);
    }
}
