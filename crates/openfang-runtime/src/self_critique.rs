//! Self-critique module — agent output quality validation.
//!
//! Implements Constitutional AI self-critique: the agent's response is evaluated
//! against a set of quality criteria before being returned to the user. If the
//! response fails critique, it can be revised in a single correction pass.
//!
//! This is Factor 10 from the AGI blueprint: agents should be able to identify
//! and correct their own mistakes before the user sees them.

use serde::{Deserialize, Serialize};

/// Quality criteria for self-critique evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CritiqueCriteria {
    /// Check for factual consistency (no contradictions within the response).
    #[serde(default = "default_true")]
    pub consistency: bool,
    /// Check for completeness (all parts of the question addressed).
    #[serde(default = "default_true")]
    pub completeness: bool,
    /// Check for safety (no harmful, biased, or dangerous content).
    #[serde(default = "default_true")]
    pub safety: bool,
    /// Check for conciseness (not unnecessarily verbose).
    #[serde(default)]
    pub conciseness: bool,
    /// Check for accuracy of any code snippets.
    #[serde(default)]
    pub code_accuracy: bool,
    /// Custom criteria (agent-specific rules from STEERING.md).
    #[serde(default)]
    pub custom: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl Default for CritiqueCriteria {
    fn default() -> Self {
        Self {
            consistency: true,
            completeness: true,
            safety: true,
            conciseness: false,
            code_accuracy: false,
            custom: Vec::new(),
        }
    }
}

/// Result of a self-critique evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CritiqueResult {
    /// Whether the response passed all criteria.
    pub passed: bool,
    /// Individual criterion evaluations.
    pub evaluations: Vec<CritiqueEvaluation>,
    /// Suggested revision (if any criteria failed).
    pub revision_prompt: Option<String>,
}

/// Evaluation of a single criterion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CritiqueEvaluation {
    /// Criterion name.
    pub criterion: String,
    /// Whether this criterion passed.
    pub passed: bool,
    /// Reasoning.
    pub reason: String,
}

/// Build a self-critique prompt that asks the LLM to evaluate its own response.
///
/// This generates a structured prompt that can be sent as a follow-up message
/// to get the LLM to critique its previous output.
pub fn build_critique_prompt(
    original_query: &str,
    response: &str,
    criteria: &CritiqueCriteria,
) -> String {
    let mut prompt = String::from(
        "Review your previous response for quality issues. Evaluate EACH criterion below and respond in this EXACT format:\n\n\
         CRITERION: <name>\nPASSED: true/false\nREASON: <brief explanation>\n\n\
         After all criteria, if ANY failed, provide:\nREVISION: <your corrected response>\n\n\
         If all passed, end with:\nVERDICT: PASS\n\n\
         Criteria to evaluate:\n",
    );

    if criteria.consistency {
        prompt.push_str("1. CONSISTENCY: Does the response contain internal contradictions?\n");
    }
    if criteria.completeness {
        prompt.push_str("2. COMPLETENESS: Does the response address all parts of the query?\n");
    }
    if criteria.safety {
        prompt.push_str(
            "3. SAFETY: Does the response avoid harmful, biased, or dangerous content?\n",
        );
    }
    if criteria.conciseness {
        prompt.push_str("4. CONCISENESS: Is the response appropriately concise without unnecessary verbosity?\n");
    }
    if criteria.code_accuracy {
        prompt.push_str(
            "5. CODE_ACCURACY: Are any code snippets syntactically and logically correct?\n",
        );
    }
    for (i, custom) in criteria.custom.iter().enumerate() {
        prompt.push_str(&format!("{}. CUSTOM: {}\n", i + 6, custom));
    }

    prompt.push_str(&format!(
        "\nOriginal query: {}\n\nResponse to evaluate:\n{}",
        truncate(original_query, 500),
        truncate(response, 3000),
    ));

    prompt
}

/// Parse a critique response from the LLM into structured form.
pub fn parse_critique_response(response: &str) -> CritiqueResult {
    let mut evaluations = Vec::new();
    let mut revision_prompt = None;
    let mut current_criterion = String::new();
    let mut current_passed = true;
    let mut current_reason = String::new();
    let mut all_passed = true;
    let mut in_revision = false;
    let mut revision_text = String::new();

    for line in response.lines() {
        let trimmed = line.trim();

        if let Some(rest) = trimmed.strip_prefix("CRITERION:") {
            // Save previous criterion if any
            if !current_criterion.is_empty() {
                evaluations.push(CritiqueEvaluation {
                    criterion: current_criterion.clone(),
                    passed: current_passed,
                    reason: current_reason.trim().to_string(),
                });
                if !current_passed {
                    all_passed = false;
                }
            }
            current_criterion = rest.trim().to_string();
            current_passed = true;
            current_reason.clear();
            in_revision = false;
        } else if let Some(rest) = trimmed.strip_prefix("PASSED:") {
            let val = rest.trim().to_lowercase();
            current_passed = val == "true" || val == "yes";
            in_revision = false;
        } else if let Some(rest) = trimmed.strip_prefix("REASON:") {
            current_reason = rest.trim().to_string();
            in_revision = false;
        } else if let Some(rest) = trimmed.strip_prefix("REVISION:") {
            in_revision = true;
            revision_text = rest.trim().to_string();
        } else if let Some(rest) = trimmed.strip_prefix("VERDICT:") {
            let verdict = rest.trim().to_uppercase();
            if verdict == "PASS" {
                all_passed = true;
            }
            in_revision = false;
        } else if in_revision {
            revision_text.push('\n');
            revision_text.push_str(trimmed);
        }
    }

    // Save last criterion
    if !current_criterion.is_empty() {
        evaluations.push(CritiqueEvaluation {
            criterion: current_criterion,
            passed: current_passed,
            reason: current_reason.trim().to_string(),
        });
        if !current_passed {
            all_passed = false;
        }
    }

    if !revision_text.is_empty() {
        revision_prompt = Some(revision_text.trim().to_string());
    }

    CritiqueResult {
        passed: all_passed,
        evaluations,
        revision_prompt,
    }
}

/// Check if a response should trigger self-critique based on heuristics.
///
/// Not all responses need critique — simple acknowledgments and short
/// responses can skip it to save tokens and latency.
pub fn should_critique(response: &str, token_threshold: usize) -> bool {
    let trimmed = response.trim();

    // Skip empty or very short responses
    if trimmed.len() < 100 {
        return false;
    }

    // Skip NO_REPLY responses
    if trimmed == "NO_REPLY" {
        return false;
    }

    // Rough token estimate (1 token ≈ 4 chars)
    let estimated_tokens = trimmed.len() / 4;
    if estimated_tokens < token_threshold {
        return false;
    }

    true
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_criteria() {
        let c = CritiqueCriteria::default();
        assert!(c.consistency);
        assert!(c.completeness);
        assert!(c.safety);
        assert!(!c.conciseness);
        assert!(!c.code_accuracy);
        assert!(c.custom.is_empty());
    }

    #[test]
    fn test_build_critique_prompt() {
        let criteria = CritiqueCriteria::default();
        let prompt = build_critique_prompt("What is Rust?", "Rust is a systems language.", &criteria);
        assert!(prompt.contains("CONSISTENCY"));
        assert!(prompt.contains("COMPLETENESS"));
        assert!(prompt.contains("SAFETY"));
        assert!(!prompt.contains("CONCISENESS"));
        assert!(prompt.contains("What is Rust?"));
        assert!(prompt.contains("Rust is a systems language."));
    }

    #[test]
    fn test_build_critique_prompt_with_custom() {
        let criteria = CritiqueCriteria {
            custom: vec!["Must include code examples".to_string()],
            ..Default::default()
        };
        let prompt = build_critique_prompt("query", "response", &criteria);
        assert!(prompt.contains("Must include code examples"));
    }

    #[test]
    fn test_parse_critique_all_pass() {
        let response = "\
CRITERION: CONSISTENCY
PASSED: true
REASON: No contradictions found.

CRITERION: COMPLETENESS
PASSED: true
REASON: All parts addressed.

VERDICT: PASS";

        let result = parse_critique_response(response);
        assert!(result.passed);
        assert_eq!(result.evaluations.len(), 2);
        assert!(result.evaluations[0].passed);
        assert!(result.evaluations[1].passed);
        assert!(result.revision_prompt.is_none());
    }

    #[test]
    fn test_parse_critique_with_failure() {
        let response = "\
CRITERION: CONSISTENCY
PASSED: true
REASON: OK

CRITERION: COMPLETENESS
PASSED: false
REASON: Did not address the second part of the question.

REVISION: Here is the corrected response that addresses both parts.";

        let result = parse_critique_response(response);
        assert!(!result.passed);
        assert_eq!(result.evaluations.len(), 2);
        assert!(result.evaluations[0].passed);
        assert!(!result.evaluations[1].passed);
        assert!(result.revision_prompt.is_some());
        assert!(result
            .revision_prompt
            .unwrap()
            .contains("corrected response"));
    }

    #[test]
    fn test_should_critique_short_response() {
        assert!(!should_critique("OK", 50));
        assert!(!should_critique("Sure, I can do that.", 50));
    }

    #[test]
    fn test_should_critique_no_reply() {
        assert!(!should_critique("NO_REPLY", 50));
    }

    #[test]
    fn test_should_critique_long_response() {
        let long = "x".repeat(500);
        assert!(should_critique(&long, 50));
    }

    #[test]
    fn test_should_critique_threshold() {
        let medium = "x".repeat(300); // ~75 tokens
        assert!(should_critique(&medium, 50));
        assert!(!should_critique(&medium, 100));
    }

    #[test]
    fn test_serde_roundtrip() {
        let criteria = CritiqueCriteria {
            conciseness: true,
            code_accuracy: true,
            custom: vec!["Must be polite".to_string()],
            ..Default::default()
        };
        let json = serde_json::to_string(&criteria).unwrap();
        let parsed: CritiqueCriteria = serde_json::from_str(&json).unwrap();
        assert!(parsed.conciseness);
        assert!(parsed.code_accuracy);
        assert_eq!(parsed.custom.len(), 1);
    }
}
