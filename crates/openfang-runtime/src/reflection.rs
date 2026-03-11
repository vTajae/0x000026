//! Reflection & meta-cognition — agent self-awareness loops.
//!
//! Blueprint Factor 12: After completing a turn, the agent can optionally
//! enter a reflection phase where it evaluates its own approach, considers
//! alternative strategies, and generates insights for future turns.
//!
//! Unlike self-critique (which checks quality criteria), reflection is
//! exploratory — the agent reasons about *why* it chose a particular path
//! and whether a different strategy would have been better.

use serde::{Deserialize, Serialize};

/// Configuration for the reflection system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionConfig {
    /// Enable reflection (default: true for complex agents, false for simple ones).
    #[serde(default)]
    pub enabled: bool,
    /// Minimum tool calls in a turn before triggering reflection.
    #[serde(default = "default_min_tool_calls")]
    pub min_tool_calls: usize,
    /// Minimum response length (chars) before triggering reflection.
    #[serde(default = "default_min_response_length")]
    pub min_response_length: usize,
    /// Maximum number of reflection insights to keep per agent.
    #[serde(default = "default_max_insights")]
    pub max_insights: usize,
    /// Whether to include reflection insights in future system prompts.
    #[serde(default = "default_inject_insights")]
    pub inject_insights: bool,
}

fn default_min_tool_calls() -> usize { 3 }
fn default_min_response_length() -> usize { 500 }
fn default_max_insights() -> usize { 10 }
fn default_inject_insights() -> bool { true }

impl Default for ReflectionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_tool_calls: default_min_tool_calls(),
            min_response_length: default_min_response_length(),
            max_insights: default_max_insights(),
            inject_insights: default_inject_insights(),
        }
    }
}

/// A reflection prompt sent to the LLM after the main turn completes.
#[derive(Debug, Clone)]
pub struct ReflectionPrompt {
    /// The user's original message.
    pub user_message: String,
    /// The agent's final response.
    pub agent_response: String,
    /// Number of tool calls made.
    pub tool_call_count: usize,
    /// Tools used (names).
    pub tools_used: Vec<String>,
    /// Whether any tool errors occurred.
    pub had_errors: bool,
    /// Total cost of this turn.
    pub cost_usd: f64,
}

/// Build the reflection prompt for the LLM.
pub fn build_reflection_prompt(ctx: &ReflectionPrompt) -> String {
    let mut prompt = String::with_capacity(800);
    prompt.push_str("Reflect on the turn you just completed. Be concise and specific.\n\n");
    prompt.push_str("## Turn Summary\n");
    prompt.push_str(&format!("- User asked: {}\n", truncate(&ctx.user_message, 200)));
    prompt.push_str(&format!("- You made {} tool calls", ctx.tool_call_count));
    if !ctx.tools_used.is_empty() {
        prompt.push_str(&format!(" ({})", ctx.tools_used.join(", ")));
    }
    prompt.push('\n');
    if ctx.had_errors {
        prompt.push_str("- Some tool calls failed\n");
    }
    prompt.push_str(&format!("- Cost: ${:.4}\n", ctx.cost_usd));
    prompt.push_str(&format!(
        "- Response length: {} chars\n\n",
        ctx.agent_response.len()
    ));

    prompt.push_str("## Answer these questions (one sentence each):\n");
    prompt.push_str("1. APPROACH: Was your strategy efficient, or could you have solved this with fewer steps?\n");
    prompt.push_str("2. ALTERNATIVES: What alternative approach could work better next time?\n");
    prompt.push_str("3. CONFIDENCE: How confident are you in the accuracy of your response (1-10)?\n");
    prompt.push_str("4. INSIGHT: What did you learn that would help with similar future requests?\n\n");
    prompt.push_str("Format your answer as:\n");
    prompt.push_str("APPROACH: <your answer>\n");
    prompt.push_str("ALTERNATIVES: <your answer>\n");
    prompt.push_str("CONFIDENCE: <1-10>\n");
    prompt.push_str("INSIGHT: <your answer>\n");

    prompt
}

/// A parsed reflection result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionInsight {
    /// Self-assessment of approach efficiency.
    pub approach: String,
    /// Alternative strategies identified.
    pub alternatives: String,
    /// Self-rated confidence (1-10).
    pub confidence: u8,
    /// Key insight for future turns.
    pub insight: String,
    /// When this reflection was generated.
    #[serde(default = "default_timestamp")]
    pub timestamp: String,
    /// Task category (derived from user message).
    pub task_category: Option<String>,
}

fn default_timestamp() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Parse a reflection response from the LLM.
pub fn parse_reflection_response(response: &str) -> Option<ReflectionInsight> {
    let mut approach = None;
    let mut alternatives = None;
    let mut confidence = None;
    let mut insight = None;

    for line in response.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("APPROACH:") {
            approach = Some(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("ALTERNATIVES:") {
            alternatives = Some(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("CONFIDENCE:") {
            let num_str = rest.trim().chars().take_while(|c| c.is_ascii_digit()).collect::<String>();
            confidence = num_str.parse::<u8>().ok().map(|n| n.min(10));
        } else if let Some(rest) = trimmed.strip_prefix("INSIGHT:") {
            insight = Some(rest.trim().to_string());
        }
    }

    Some(ReflectionInsight {
        approach: approach.unwrap_or_else(|| "No approach assessment".into()),
        alternatives: alternatives.unwrap_or_else(|| "No alternatives identified".into()),
        confidence: confidence.unwrap_or(5),
        insight: insight.unwrap_or_else(|| "No insight captured".into()),
        timestamp: default_timestamp(),
        task_category: None,
    })
}

/// Determine whether a turn warrants reflection.
pub fn should_reflect(config: &ReflectionConfig, tool_call_count: usize, response_len: usize) -> bool {
    if !config.enabled {
        return false;
    }
    tool_call_count >= config.min_tool_calls || response_len >= config.min_response_length
}

/// Categorize a user message into a task type for organizing insights.
pub fn categorize_task(user_message: &str) -> &'static str {
    let lower = user_message.to_lowercase();
    if lower.contains("bug") || lower.contains("fix") || lower.contains("error") || lower.contains("broken") {
        "debugging"
    } else if lower.contains("implement") || lower.contains("add") || lower.contains("create") || lower.contains("build") {
        "implementation"
    } else if lower.contains("refactor") || lower.contains("clean") || lower.contains("improve") {
        "refactoring"
    } else if lower.contains("explain") || lower.contains("how") || lower.contains("what") || lower.contains("why") {
        "explanation"
    } else if lower.contains("test") || lower.contains("verify") || lower.contains("check") {
        "testing"
    } else if lower.contains("search") || lower.contains("find") || lower.contains("look") {
        "research"
    } else {
        "general"
    }
}

/// Format stored insights as a prompt section for injection into system prompts.
pub fn insights_to_prompt_section(insights: &[ReflectionInsight]) -> Option<String> {
    if insights.is_empty() {
        return None;
    }
    let mut out = String::from("## Reflection Insights (from previous turns)\n");
    for (i, ins) in insights.iter().enumerate().rev().take(5) {
        out.push_str(&format!(
            "{}. [Confidence: {}/10] {}\n",
            i + 1,
            ins.confidence,
            ins.insight
        ));
        if ins.alternatives != "No alternatives identified" {
            out.push_str(&format!("   Alternative: {}\n", ins.alternatives));
        }
    }
    Some(out)
}

/// Manage a rolling buffer of reflection insights.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InsightStore {
    pub insights: Vec<ReflectionInsight>,
    pub max_size: usize,
}

impl InsightStore {
    pub fn new(max_size: usize) -> Self {
        Self {
            insights: Vec::new(),
            max_size,
        }
    }

    /// Add an insight, evicting the oldest if at capacity.
    pub fn add(&mut self, mut insight: ReflectionInsight, user_message: &str) {
        insight.task_category = Some(categorize_task(user_message).to_string());
        if self.insights.len() >= self.max_size {
            self.insights.remove(0);
        }
        self.insights.push(insight);
    }

    /// Get insights filtered by task category.
    pub fn for_category(&self, category: &str) -> Vec<&ReflectionInsight> {
        self.insights
            .iter()
            .filter(|i| i.task_category.as_deref() == Some(category))
            .collect()
    }

    /// Get the most recent N insights.
    pub fn recent(&self, n: usize) -> &[ReflectionInsight] {
        let start = self.insights.len().saturating_sub(n);
        &self.insights[start..]
    }

    /// Average confidence across all insights.
    pub fn avg_confidence(&self) -> f64 {
        if self.insights.is_empty() {
            return 0.0;
        }
        let sum: u64 = self.insights.iter().map(|i| i.confidence as u64).sum();
        sum as f64 / self.insights.len() as f64
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.min(s.len())])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_reflect_disabled() {
        let config = ReflectionConfig::default();
        assert!(!should_reflect(&config, 10, 1000));
    }

    #[test]
    fn test_should_reflect_by_tool_calls() {
        let config = ReflectionConfig {
            enabled: true,
            ..Default::default()
        };
        assert!(!should_reflect(&config, 1, 100));
        assert!(should_reflect(&config, 5, 100));
    }

    #[test]
    fn test_should_reflect_by_response_length() {
        let config = ReflectionConfig {
            enabled: true,
            ..Default::default()
        };
        assert!(should_reflect(&config, 0, 600));
    }

    #[test]
    fn test_build_reflection_prompt() {
        let ctx = ReflectionPrompt {
            user_message: "Fix the login bug".into(),
            agent_response: "I fixed the issue by...".into(),
            tool_call_count: 5,
            tools_used: vec!["file_read".into(), "shell_exec".into()],
            had_errors: true,
            cost_usd: 0.003,
        };
        let prompt = build_reflection_prompt(&ctx);
        assert!(prompt.contains("Fix the login bug"));
        assert!(prompt.contains("5 tool calls"));
        assert!(prompt.contains("file_read"));
        assert!(prompt.contains("APPROACH"));
        assert!(prompt.contains("ALTERNATIVES"));
        assert!(prompt.contains("CONFIDENCE"));
        assert!(prompt.contains("INSIGHT"));
    }

    #[test]
    fn test_parse_reflection_response() {
        let response = "\
APPROACH: My strategy was efficient, I found the bug in 2 steps.
ALTERNATIVES: Could have used grep instead of reading files sequentially.
CONFIDENCE: 8
INSIGHT: Login bugs often relate to session cookie handling.";
        let insight = parse_reflection_response(response).unwrap();
        assert!(insight.approach.contains("efficient"));
        assert!(insight.alternatives.contains("grep"));
        assert_eq!(insight.confidence, 8);
        assert!(insight.insight.contains("cookie"));
    }

    #[test]
    fn test_parse_reflection_partial() {
        let response = "CONFIDENCE: 6\nINSIGHT: Always check env vars first.";
        let insight = parse_reflection_response(response).unwrap();
        assert_eq!(insight.confidence, 6);
        assert!(insight.insight.contains("env vars"));
        assert_eq!(insight.approach, "No approach assessment");
    }

    #[test]
    fn test_categorize_task() {
        assert_eq!(categorize_task("Fix the login bug"), "debugging");
        assert_eq!(categorize_task("Add a new endpoint"), "implementation");
        assert_eq!(categorize_task("Refactor the parser"), "refactoring");
        assert_eq!(categorize_task("Explain how routing works"), "explanation");
        assert_eq!(categorize_task("Test the API"), "testing");
        assert_eq!(categorize_task("Find the config file"), "research");
        assert_eq!(categorize_task("Hello world"), "general");
    }

    #[test]
    fn test_insight_store() {
        let mut store = InsightStore::new(3);
        for i in 0..5 {
            let insight = ReflectionInsight {
                approach: format!("approach {i}"),
                alternatives: "alt".into(),
                confidence: (i + 5) as u8,
                insight: format!("insight {i}"),
                timestamp: "2026-01-01T00:00:00Z".into(),
                task_category: None,
            };
            store.add(insight, "Fix bug");
        }
        // Should have evicted oldest to stay at max 3
        assert_eq!(store.insights.len(), 3);
        assert!(store.insights[0].approach.contains("2"));
    }

    #[test]
    fn test_insight_store_category_filter() {
        let mut store = InsightStore::new(10);
        store.add(
            ReflectionInsight {
                approach: "a".into(),
                alternatives: "b".into(),
                confidence: 7,
                insight: "debug insight".into(),
                timestamp: String::new(),
                task_category: None,
            },
            "Fix a bug",
        );
        store.add(
            ReflectionInsight {
                approach: "a".into(),
                alternatives: "b".into(),
                confidence: 8,
                insight: "impl insight".into(),
                timestamp: String::new(),
                task_category: None,
            },
            "Add a feature",
        );
        let debug_insights = store.for_category("debugging");
        assert_eq!(debug_insights.len(), 1);
        assert!(debug_insights[0].insight.contains("debug"));
    }

    #[test]
    fn test_avg_confidence() {
        let mut store = InsightStore::new(10);
        for conf in [6, 8, 10] {
            store.add(
                ReflectionInsight {
                    approach: String::new(),
                    alternatives: String::new(),
                    confidence: conf,
                    insight: String::new(),
                    timestamp: String::new(),
                    task_category: None,
                },
                "task",
            );
        }
        assert!((store.avg_confidence() - 8.0).abs() < 0.01);
    }

    #[test]
    fn test_insights_to_prompt_section_empty() {
        assert!(insights_to_prompt_section(&[]).is_none());
    }

    #[test]
    fn test_insights_to_prompt_section() {
        let insights = vec![ReflectionInsight {
            approach: "good".into(),
            alternatives: "use cache".into(),
            confidence: 9,
            insight: "Caching improves performance".into(),
            timestamp: String::new(),
            task_category: None,
        }];
        let section = insights_to_prompt_section(&insights).unwrap();
        assert!(section.contains("Reflection Insights"));
        assert!(section.contains("9/10"));
        assert!(section.contains("Caching"));
        assert!(section.contains("use cache"));
    }

    #[test]
    fn test_confidence_clamped() {
        let response = "CONFIDENCE: 15";
        let insight = parse_reflection_response(response).unwrap();
        assert_eq!(insight.confidence, 10);
    }

    #[test]
    fn test_serde_roundtrip() {
        let insight = ReflectionInsight {
            approach: "efficient".into(),
            alternatives: "none".into(),
            confidence: 7,
            insight: "check logs first".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            task_category: Some("debugging".into()),
        };
        let json = serde_json::to_string(&insight).unwrap();
        let parsed: ReflectionInsight = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.confidence, 7);
        assert_eq!(parsed.task_category.as_deref(), Some("debugging"));
    }
}
