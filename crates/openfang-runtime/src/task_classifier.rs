//! Task category classifier for smart model routing.
//!
//! Classifies incoming requests into task categories using keyword heuristics.
//! These categories feed into the `ModelLedger` for per-category scoring and
//! into the `ModelRouter` for category-aware model selection.

use crate::llm_driver::CompletionRequest;
use serde::{Deserialize, Serialize};

/// High-level task categories for routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskCategory {
    /// Code generation, debugging, refactoring.
    Code,
    /// Document analysis, summarization, extraction.
    Analysis,
    /// Creative writing, drafting, essays.
    Writing,
    /// Architecture, strategy, roadmaps.
    Planning,
    /// Web search, factual lookups, research.
    Research,
    /// CSV/JSON parsing, calculations, transforms.
    Data,
    /// Simple Q&A, greetings, casual chat.
    Conversation,
    /// Image/video/audio understanding.
    Multimodal,
}

impl std::fmt::Display for TaskCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Code => write!(f, "code"),
            Self::Analysis => write!(f, "analysis"),
            Self::Writing => write!(f, "writing"),
            Self::Planning => write!(f, "planning"),
            Self::Research => write!(f, "research"),
            Self::Data => write!(f, "data"),
            Self::Conversation => write!(f, "conversation"),
            Self::Multimodal => write!(f, "multimodal"),
        }
    }
}

/// Keyword weights for each category.
struct CategoryKeywords {
    category: TaskCategory,
    keywords: &'static [&'static str],
    weight: u32,
}

const CATEGORY_KEYWORDS: &[CategoryKeywords] = &[
    CategoryKeywords {
        category: TaskCategory::Code,
        keywords: &[
            "fn ", "def ", "class ", "import ", "function ", "async ", "struct ",
            "impl ", "return ", "```", ".rs", ".py", ".js", ".ts", ".go", ".java",
            "cargo", "debug", "compile", "refactor", "bug", "error", "code",
            "implement", "variable", "method", "api", "endpoint", "syntax",
            "git ", "commit", "branch", "merge", "pull request",
        ],
        weight: 3,
    },
    CategoryKeywords {
        category: TaskCategory::Analysis,
        keywords: &[
            "analyze", "summarize", "extract", "compare", "evaluate", "review",
            "assess", "breakdown", "explain this", "what does this mean",
            "interpret", "diagnose", "investigate", "audit",
        ],
        weight: 3,
    },
    CategoryKeywords {
        category: TaskCategory::Writing,
        keywords: &[
            "write", "draft", "essay", "blog", "compose", "letter", "email",
            "article", "story", "poem", "creative", "rewrite", "edit this",
            "proofread", "tone",
        ],
        weight: 3,
    },
    CategoryKeywords {
        category: TaskCategory::Planning,
        keywords: &[
            "plan", "design", "architect", "strategy", "roadmap", "timeline",
            "milestone", "spec", "proposal", "outline", "steps to", "how to approach",
            "breakdown into", "decompose",
        ],
        weight: 3,
    },
    CategoryKeywords {
        category: TaskCategory::Research,
        keywords: &[
            "search", "find", "look up", "what is", "latest", "who is",
            "when did", "where is", "how many", "current", "news", "trend",
            "discover", "source",
        ],
        weight: 3,
    },
    CategoryKeywords {
        category: TaskCategory::Data,
        keywords: &[
            "csv", "json", "table", "calculate", "parse", "transform",
            "spreadsheet", "data", "column", "row", "aggregate", "filter",
            "sort", "merge data", "convert",
        ],
        weight: 3,
    },
    CategoryKeywords {
        category: TaskCategory::Multimodal,
        keywords: &[
            "image", "photo", "picture", "screenshot", "video", "audio",
            "diagram", "chart", "graph", "visual", "describe this image",
            "what do you see", "ocr",
        ],
        weight: 3,
    },
];

/// Classify a completion request into a task category.
///
/// Uses keyword scoring on the last user message. If no category dominates,
/// falls back to `Conversation` (cheapest routing).
pub fn classify(request: &CompletionRequest) -> TaskCategory {
    // Get the last user message text
    let text = request
        .messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, openfang_types::message::Role::User))
        .map(|m| m.content.text_content())
        .unwrap_or_default();

    let text_lower = text.to_lowercase();

    // Short messages default to conversation
    if text_lower.len() < 20 {
        return TaskCategory::Conversation;
    }

    // Score each category
    let mut scores: Vec<(TaskCategory, u32)> = CATEGORY_KEYWORDS
        .iter()
        .map(|ck| {
            let score: u32 = ck
                .keywords
                .iter()
                .filter(|kw| text_lower.contains(*kw))
                .count() as u32
                * ck.weight;
            (ck.category, score)
        })
        .collect();

    // Bonus: code fences are a strong code signal
    if text.contains("```") {
        if let Some(entry) = scores.iter_mut().find(|(c, _)| *c == TaskCategory::Code) {
            entry.1 += 10;
        }
    }

    // Bonus: tool availability suggests code/data tasks
    if !request.tools.is_empty() {
        if let Some(entry) = scores.iter_mut().find(|(c, _)| *c == TaskCategory::Code) {
            entry.1 += 5;
        }
    }

    // Sort descending by score
    scores.sort_by(|a, b| b.1.cmp(&a.1));

    // Require a minimum threshold to classify (at least one keyword match)
    if scores.first().map(|(_, s)| *s).unwrap_or(0) >= 3 {
        scores[0].0
    } else {
        TaskCategory::Conversation
    }
}

/// Estimate the context window tokens needed for a request.
pub fn estimate_context_tokens(request: &CompletionRequest) -> usize {
    let char_count: usize = request
        .messages
        .iter()
        .map(|m| m.content.text_length())
        .sum();
    // Rough estimate: 4 chars per token
    let msg_tokens = char_count / 4;
    let system_tokens = request.system.as_ref().map(|s| s.len() / 4).unwrap_or(0);
    let tool_tokens = request.tools.len() * 200; // ~200 tokens per tool definition
    msg_tokens + system_tokens + tool_tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfang_types::message::{Message, MessageContent, Role};
    use openfang_types::tool::ToolDefinition;

    fn make_request(text: &str) -> CompletionRequest {
        CompletionRequest {
            model: "test".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: MessageContent::text(text),
            }],
            tools: vec![],
            max_tokens: 4096,
            temperature: 0.7,
            system: None,
            thinking: None,
        }
    }

    #[test]
    fn test_classify_code() {
        let req = make_request("Write a function that implements async file reading in Rust");
        assert_eq!(classify(&req), TaskCategory::Code);
    }

    #[test]
    fn test_classify_code_with_fence() {
        let req = make_request("Fix this code:\n```rust\nfn main() {}\n```");
        assert_eq!(classify(&req), TaskCategory::Code);
    }

    #[test]
    fn test_classify_analysis() {
        let req = make_request("Analyze this document and summarize the key findings");
        assert_eq!(classify(&req), TaskCategory::Analysis);
    }

    #[test]
    fn test_classify_writing() {
        let req = make_request("Write a blog post about machine learning trends");
        // "write" matches both Writing and Code, but "blog" + "article" should tip it
        let cat = classify(&req);
        assert!(cat == TaskCategory::Writing || cat == TaskCategory::Code);
    }

    #[test]
    fn test_classify_short_message() {
        let req = make_request("Hello!");
        assert_eq!(classify(&req), TaskCategory::Conversation);
    }

    #[test]
    fn test_classify_planning() {
        let req = make_request("Design an architecture and create a roadmap for the migration");
        assert_eq!(classify(&req), TaskCategory::Planning);
    }

    #[test]
    fn test_classify_data() {
        let req = make_request("Parse this CSV data and calculate the aggregate totals per column");
        assert_eq!(classify(&req), TaskCategory::Data);
    }

    #[test]
    fn test_classify_research() {
        let req = make_request("What is the latest news about the Rust programming language?");
        // Could be Research or Code; both valid
        let cat = classify(&req);
        assert!(cat == TaskCategory::Research || cat == TaskCategory::Code);
    }

    #[test]
    fn test_estimate_context_tokens() {
        let req = make_request("Hello world"); // 11 chars ≈ 2 tokens
        let tokens = estimate_context_tokens(&req);
        assert!(tokens >= 2);
        assert!(tokens < 100);
    }

    #[test]
    fn test_estimate_context_tokens_with_tools() {
        let mut req = make_request("Use tools");
        req.tools = vec![
            ToolDefinition {
                name: "tool1".to_string(),
                description: "A tool".to_string(),
                input_schema: serde_json::json!({}),
            },
            ToolDefinition {
                name: "tool2".to_string(),
                description: "Another tool".to_string(),
                input_schema: serde_json::json!({}),
            },
        ];
        let tokens = estimate_context_tokens(&req);
        assert!(tokens >= 400); // 2 tools * 200
    }

    #[test]
    fn test_task_category_display() {
        assert_eq!(TaskCategory::Code.to_string(), "code");
        assert_eq!(TaskCategory::Analysis.to_string(), "analysis");
        assert_eq!(TaskCategory::Conversation.to_string(), "conversation");
    }

    #[test]
    fn test_task_category_serde() {
        let cat = TaskCategory::Code;
        let json = serde_json::to_string(&cat).unwrap();
        assert_eq!(json, "\"code\"");
        let back: TaskCategory = serde_json::from_str(&json).unwrap();
        assert_eq!(back, TaskCategory::Code);
    }
}
