//! Curriculum learning — progressive skill acquisition.
//!
//! Blueprint Factor 13: Agents progress through skill tiers, mastering
//! simpler capabilities before unlocking advanced ones. The system tracks
//! per-skill mastery scores, gates tool access based on demonstrated
//! competence, and suggests next skills to develop.
//!
//! Mastery is measured by success rate, cost efficiency, and consistency
//! across multiple invocations of a skill/tool.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Skill tier levels — each tier unlocks after mastering the previous one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillTier {
    /// Basic operations: file read, simple shell, text response.
    Foundational = 0,
    /// Multi-step tool chains, web fetch, memory operations.
    Intermediate = 1,
    /// Agent spawning, code generation, complex reasoning.
    Advanced = 2,
    /// Autonomous multi-agent orchestration, self-modification.
    Expert = 3,
}

impl SkillTier {
    /// Mastery threshold (0.0-1.0) required to advance from this tier.
    pub fn mastery_threshold(&self) -> f64 {
        match self {
            Self::Foundational => 0.7,
            Self::Intermediate => 0.75,
            Self::Advanced => 0.8,
            Self::Expert => 0.85,
        }
    }

    /// Next tier, if one exists.
    pub fn next(&self) -> Option<Self> {
        match self {
            Self::Foundational => Some(Self::Intermediate),
            Self::Intermediate => Some(Self::Advanced),
            Self::Advanced => Some(Self::Expert),
            Self::Expert => None,
        }
    }

    /// All tiers in order.
    pub fn all() -> &'static [Self] {
        &[
            Self::Foundational,
            Self::Intermediate,
            Self::Advanced,
            Self::Expert,
        ]
    }
}

/// Mapping of tool names to their required skill tier.
pub fn tool_tier(tool_name: &str) -> SkillTier {
    let n = tool_name.to_lowercase();

    // Foundational — basic read/respond tools
    if n.starts_with("file_read")
        || n.starts_with("file_list")
        || n.starts_with("directory_list")
        || n == "memory_recall"
        || n == "knowledge_query"
    {
        return SkillTier::Foundational;
    }

    // Intermediate — write, fetch, multi-step
    if n.starts_with("file_write")
        || n.starts_with("file_edit")
        || n.starts_with("web_fetch")
        || n.starts_with("web_search")
        || n == "shell_exec"
        || n.starts_with("memory_store")
        || n.starts_with("link_")
    {
        return SkillTier::Intermediate;
    }

    // Advanced — generation, browser automation, containers
    if n.starts_with("image_")
        || n.starts_with("browser_")
        || n.starts_with("playwright_")
        || n.starts_with("container_")
        || n.starts_with("docker_")
        || n == "tts_speak"
        || n.starts_with("cron_")
        || n.starts_with("hand_")
    {
        return SkillTier::Advanced;
    }

    // Expert — agent orchestration, task delegation
    if n.starts_with("agent_")
        || n.starts_with("task_")
        || n == "exec_workflow"
        || n == "self_modify"
    {
        return SkillTier::Expert;
    }

    // Default to intermediate for unknown tools
    SkillTier::Intermediate
}

/// Per-tool performance tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMastery {
    /// Total invocations.
    pub total_calls: u64,
    /// Successful invocations (no error).
    pub success_count: u64,
    /// Average cost per call in USD.
    pub avg_cost: f64,
    /// Recent success streak (consecutive successes).
    pub streak: u32,
    /// Best streak achieved.
    pub best_streak: u32,
}

impl Default for ToolMastery {
    fn default() -> Self {
        Self {
            total_calls: 0,
            success_count: 0,
            avg_cost: 0.0,
            streak: 0,
            best_streak: 0,
        }
    }
}

impl ToolMastery {
    /// Success rate (0.0 - 1.0).
    pub fn success_rate(&self) -> f64 {
        if self.total_calls == 0 {
            return 0.0;
        }
        self.success_count as f64 / self.total_calls as f64
    }

    /// Mastery score combining success rate and consistency (streak).
    pub fn mastery_score(&self) -> f64 {
        if self.total_calls < 3 {
            return 0.0; // Not enough data
        }
        let rate = self.success_rate();
        let consistency = (self.best_streak as f64 / self.total_calls as f64).min(1.0);
        // 70% success rate, 30% consistency
        rate * 0.7 + consistency * 0.3
    }

    /// Record a tool call result.
    pub fn record(&mut self, success: bool, cost_usd: f64) {
        self.total_calls += 1;
        if success {
            self.success_count += 1;
            self.streak += 1;
            if self.streak > self.best_streak {
                self.best_streak = self.streak;
            }
        } else {
            self.streak = 0;
        }
        // Running average cost
        let n = self.total_calls as f64;
        self.avg_cost = self.avg_cost * (n - 1.0) / n + cost_usd / n;
    }
}

/// Agent-level curriculum state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurriculumState {
    /// Current highest unlocked tier.
    pub current_tier: SkillTier,
    /// Per-tool mastery tracking.
    pub tool_mastery: HashMap<String, ToolMastery>,
    /// Total turns completed.
    pub total_turns: u64,
    /// Tier advancement history.
    pub tier_history: Vec<TierAdvancement>,
}

/// Record of a tier advancement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierAdvancement {
    pub from_tier: SkillTier,
    pub to_tier: SkillTier,
    pub turns_taken: u64,
    pub timestamp: String,
}

impl Default for CurriculumState {
    fn default() -> Self {
        Self {
            current_tier: SkillTier::Foundational,
            tool_mastery: HashMap::new(),
            total_turns: 0,
            tier_history: Vec::new(),
        }
    }
}

impl CurriculumState {
    /// Record a tool call and update mastery.
    pub fn record_tool_call(&mut self, tool_name: &str, success: bool, cost_usd: f64) {
        self.tool_mastery
            .entry(tool_name.to_string())
            .or_default()
            .record(success, cost_usd);
    }

    /// Record a completed turn.
    pub fn record_turn(&mut self) {
        self.total_turns += 1;
    }

    /// Check if the agent qualifies to advance to the next tier.
    pub fn check_advancement(&mut self) -> Option<SkillTier> {
        let next = self.current_tier.next()?;
        let threshold = self.current_tier.mastery_threshold();

        // Get all tools at the current tier
        let tier_tools: Vec<&String> = self
            .tool_mastery
            .keys()
            .filter(|name| tool_tier(name) == self.current_tier)
            .collect();

        // Need at least 2 tools with enough data
        let qualified: Vec<f64> = tier_tools
            .iter()
            .filter_map(|name| {
                let m = self.tool_mastery.get(*name)?;
                if m.total_calls >= 5 {
                    Some(m.mastery_score())
                } else {
                    None
                }
            })
            .collect();

        if qualified.len() < 2 {
            return None;
        }

        // Average mastery must exceed threshold
        let avg: f64 = qualified.iter().sum::<f64>() / qualified.len() as f64;
        if avg >= threshold {
            let advancement = TierAdvancement {
                from_tier: self.current_tier,
                to_tier: next,
                turns_taken: self.total_turns,
                timestamp: chrono::Utc::now().to_rfc3339(),
            };
            self.tier_history.push(advancement);
            self.current_tier = next;
            Some(next)
        } else {
            None
        }
    }

    /// Check if a tool is accessible at the agent's current tier.
    pub fn can_use_tool(&self, tool_name: &str) -> bool {
        tool_tier(tool_name) <= self.current_tier
    }

    /// Get tools the agent should focus on mastering next.
    pub fn suggested_practice(&self) -> Vec<String> {
        let threshold = self.current_tier.mastery_threshold();
        let mut suggestions = Vec::new();

        for (name, mastery) in &self.tool_mastery {
            if tool_tier(name) == self.current_tier && mastery.mastery_score() < threshold {
                suggestions.push(name.clone());
            }
        }

        suggestions.sort();
        suggestions
    }

    /// Overall mastery score for current tier (0.0-1.0).
    pub fn tier_mastery(&self) -> f64 {
        let scores: Vec<f64> = self
            .tool_mastery
            .iter()
            .filter(|(name, m)| tool_tier(name) == self.current_tier && m.total_calls >= 3)
            .map(|(_, m)| m.mastery_score())
            .collect();

        if scores.is_empty() {
            return 0.0;
        }
        scores.iter().sum::<f64>() / scores.len() as f64
    }

    /// Format curriculum status as a prompt section.
    pub fn to_prompt_section(&self) -> String {
        let mut out = format!(
            "## Skill Level: {:?}\n",
            self.current_tier
        );
        out.push_str(&format!(
            "Tier mastery: {:.0}% (need {:.0}% to advance)\n",
            self.tier_mastery() * 100.0,
            self.current_tier.mastery_threshold() * 100.0
        ));

        let suggestions = self.suggested_practice();
        if !suggestions.is_empty() {
            out.push_str(&format!("Practice focus: {}\n", suggestions.join(", ")));
        }

        if let Some(next) = self.current_tier.next() {
            out.push_str(&format!("Next tier: {:?}\n", next));
        } else {
            out.push_str("Maximum tier reached.\n");
        }

        out
    }
}

/// Filter a tool list based on curriculum gating.
/// Returns only tools the agent has unlocked.
pub fn gate_tools(
    tools: &[openfang_types::tool::ToolDefinition],
    state: &CurriculumState,
) -> Vec<openfang_types::tool::ToolDefinition> {
    tools
        .iter()
        .filter(|t| state.can_use_tool(&t.name))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skill_tier_ordering() {
        assert!(SkillTier::Foundational < SkillTier::Intermediate);
        assert!(SkillTier::Intermediate < SkillTier::Advanced);
        assert!(SkillTier::Advanced < SkillTier::Expert);
    }

    #[test]
    fn test_tier_next() {
        assert_eq!(SkillTier::Foundational.next(), Some(SkillTier::Intermediate));
        assert_eq!(SkillTier::Expert.next(), None);
    }

    #[test]
    fn test_tool_tier_mapping() {
        assert_eq!(tool_tier("file_read"), SkillTier::Foundational);
        assert_eq!(tool_tier("web_fetch"), SkillTier::Intermediate);
        assert_eq!(tool_tier("browser_navigate"), SkillTier::Advanced);
        assert_eq!(tool_tier("agent_spawn"), SkillTier::Expert);
        // Unknown tools default to intermediate
        assert_eq!(tool_tier("custom_tool"), SkillTier::Intermediate);
    }

    #[test]
    fn test_tool_mastery_record() {
        let mut m = ToolMastery::default();
        m.record(true, 0.001);
        m.record(true, 0.002);
        m.record(false, 0.001);
        assert_eq!(m.total_calls, 3);
        assert_eq!(m.success_count, 2);
        assert_eq!(m.streak, 0); // reset by failure
        assert_eq!(m.best_streak, 2);
    }

    #[test]
    fn test_mastery_score_insufficient_data() {
        let mut m = ToolMastery::default();
        m.record(true, 0.001);
        assert_eq!(m.mastery_score(), 0.0); // < 3 calls
    }

    #[test]
    fn test_mastery_score_perfect() {
        let mut m = ToolMastery::default();
        for _ in 0..10 {
            m.record(true, 0.001);
        }
        assert!(m.mastery_score() > 0.9);
    }

    #[test]
    fn test_success_rate() {
        let mut m = ToolMastery::default();
        m.record(true, 0.0);
        m.record(true, 0.0);
        m.record(false, 0.0);
        m.record(true, 0.0);
        assert!((m.success_rate() - 0.75).abs() < 0.01);
    }

    #[test]
    fn test_curriculum_state_default() {
        let state = CurriculumState::default();
        assert_eq!(state.current_tier, SkillTier::Foundational);
        assert!(state.can_use_tool("file_read"));
        assert!(!state.can_use_tool("agent_spawn"));
    }

    #[test]
    fn test_can_use_tool_gating() {
        let mut state = CurriculumState::default();
        assert!(state.can_use_tool("file_read")); // foundational
        assert!(!state.can_use_tool("web_fetch")); // intermediate, not unlocked

        state.current_tier = SkillTier::Intermediate;
        assert!(state.can_use_tool("web_fetch"));
        assert!(!state.can_use_tool("browser_navigate")); // advanced
    }

    #[test]
    fn test_advancement() {
        let mut state = CurriculumState::default();
        // Build up mastery on foundational tools
        for _ in 0..10 {
            state.record_tool_call("file_read", true, 0.001);
            state.record_tool_call("file_list", true, 0.001);
        }
        state.record_turn();

        let advanced = state.check_advancement();
        assert_eq!(advanced, Some(SkillTier::Intermediate));
        assert_eq!(state.current_tier, SkillTier::Intermediate);
        assert_eq!(state.tier_history.len(), 1);
    }

    #[test]
    fn test_no_advancement_insufficient_mastery() {
        let mut state = CurriculumState::default();
        // Mix of success and failure — not enough mastery
        for _ in 0..5 {
            state.record_tool_call("file_read", true, 0.001);
            state.record_tool_call("file_read", false, 0.001);
            state.record_tool_call("file_list", true, 0.001);
            state.record_tool_call("file_list", false, 0.001);
        }

        let advanced = state.check_advancement();
        assert_eq!(advanced, None);
        assert_eq!(state.current_tier, SkillTier::Foundational);
    }

    #[test]
    fn test_suggested_practice() {
        let mut state = CurriculumState::default();
        // One tool mastered, one not
        for _ in 0..10 {
            state.record_tool_call("file_read", true, 0.001);
        }
        for _ in 0..5 {
            state.record_tool_call("file_list", true, 0.001);
            state.record_tool_call("file_list", false, 0.001);
        }
        let suggestions = state.suggested_practice();
        assert!(suggestions.contains(&"file_list".to_string()));
    }

    #[test]
    fn test_prompt_section() {
        let state = CurriculumState::default();
        let section = state.to_prompt_section();
        assert!(section.contains("Foundational"));
        assert!(section.contains("Next tier: Intermediate"));
    }

    #[test]
    fn test_serde_roundtrip() {
        let mut state = CurriculumState::default();
        state.record_tool_call("file_read", true, 0.001);
        state.record_turn();
        let json = serde_json::to_string(&state).unwrap();
        let parsed: CurriculumState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.total_turns, 1);
        assert!(parsed.tool_mastery.contains_key("file_read"));
    }

    #[test]
    fn test_gate_tools() {
        use openfang_types::tool::ToolDefinition;
        let tools = vec![
            ToolDefinition {
                name: "file_read".into(),
                description: "Read a file".into(),
                input_schema: serde_json::json!({}),
            },
            ToolDefinition {
                name: "agent_spawn".into(),
                description: "Spawn agent".into(),
                input_schema: serde_json::json!({}),
            },
        ];
        let state = CurriculumState::default(); // Foundational tier
        let gated = gate_tools(&tools, &state);
        assert_eq!(gated.len(), 1);
        assert_eq!(gated[0].name, "file_read");
    }

    #[test]
    fn test_tier_mastery_empty() {
        let state = CurriculumState::default();
        assert_eq!(state.tier_mastery(), 0.0);
    }
}
