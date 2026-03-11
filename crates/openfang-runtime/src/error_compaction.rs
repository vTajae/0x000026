//! Error compaction — preserve failure context without bloating the window.
//!
//! Factor 4 from the AGI blueprint: "Compact Errors into Context."
//!
//! When tool calls fail or errors occur during the agent loop, the full
//! error output is often verbose (stack traces, HTML error pages, etc.).
//! This module compacts those errors into concise, structured summaries
//! that retain the diagnostic value while using minimal tokens.
//!
//! The compacted errors are injected as tool_result messages so the LLM
//! can learn from failures without context window pressure.

use serde::{Deserialize, Serialize};

/// A compacted error summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactedError {
    /// Tool that failed (if applicable).
    pub tool_name: Option<String>,
    /// Error category.
    pub category: ErrorCategory,
    /// One-line summary.
    pub summary: String,
    /// Key details extracted from the full error.
    pub details: Vec<String>,
    /// Suggested recovery action.
    pub suggestion: Option<String>,
}

/// Categories of errors for structured handling.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    /// Network/HTTP errors (timeout, DNS, connection refused).
    Network,
    /// Permission denied, auth failures.
    Permission,
    /// File not found, path errors.
    NotFound,
    /// Rate limiting, quota exceeded.
    RateLimit,
    /// Invalid input or parameters.
    Validation,
    /// Internal/unexpected errors.
    Internal,
    /// Timeout (tool took too long).
    Timeout,
    /// Shell command failures.
    ShellError,
    /// API errors from external services.
    ApiError,
    /// Unknown/uncategorized.
    Unknown,
}

/// Compact a verbose error message into a structured, token-efficient summary.
pub fn compact_error(tool_name: Option<&str>, error: &str) -> CompactedError {
    let category = categorize_error(error);
    let summary = extract_summary(error);
    let details = extract_key_details(error);
    let suggestion = suggest_recovery(&category, tool_name, error);

    CompactedError {
        tool_name: tool_name.map(|s| s.to_string()),
        category,
        summary,
        details,
        suggestion,
    }
}

/// Format a compacted error as a concise string for injection into messages.
pub fn format_compact_error(error: &CompactedError) -> String {
    let mut out = String::with_capacity(200);
    if let Some(ref tool) = error.tool_name {
        out.push_str(&format!("[{tool}] "));
    }
    out.push_str(&format!("{:?}: {}", error.category, error.summary));
    for detail in &error.details {
        out.push_str(&format!("\n  - {detail}"));
    }
    if let Some(ref suggestion) = error.suggestion {
        out.push_str(&format!("\n  -> {suggestion}"));
    }
    out
}

/// Compact a tool result error message in-place, preserving the diagnostic
/// value while reducing token count.
///
/// Returns the original length and compacted length for metrics.
pub fn compact_tool_error(error_text: &str, max_chars: usize) -> String {
    if error_text.len() <= max_chars {
        return error_text.to_string();
    }

    let compacted = compact_error(None, error_text);
    let formatted = format_compact_error(&compacted);

    if formatted.len() <= max_chars {
        formatted
    } else {
        // Still too long — hard truncate with ellipsis
        let end = error_text
            .char_indices()
            .nth(max_chars.saturating_sub(20))
            .map(|(i, _)| i)
            .unwrap_or(error_text.len());
        format!("{}... [truncated, was {} chars]", &error_text[..end], error_text.len())
    }
}

/// Categorize an error string into a structured category.
fn categorize_error(error: &str) -> ErrorCategory {
    let lower = error.to_lowercase();

    if lower.contains("timeout") || lower.contains("timed out") || lower.contains("deadline") {
        return ErrorCategory::Timeout;
    }
    if lower.contains("rate limit") || lower.contains("429") || lower.contains("quota") || lower.contains("too many requests") {
        return ErrorCategory::RateLimit;
    }
    if lower.contains("permission denied") || lower.contains("403") || lower.contains("unauthorized") || lower.contains("401") {
        return ErrorCategory::Permission;
    }
    if lower.contains("not found") || lower.contains("404") || lower.contains("no such file") || lower.contains("enoent") {
        return ErrorCategory::NotFound;
    }
    if lower.contains("connection refused") || lower.contains("dns") || lower.contains("network") || lower.contains("econnrefused") {
        return ErrorCategory::Network;
    }
    if lower.contains("invalid") || lower.contains("bad request") || lower.contains("400") || lower.contains("validation") {
        return ErrorCategory::Validation;
    }
    if lower.contains("exit code") || lower.contains("command failed") || lower.contains("non-zero") {
        return ErrorCategory::ShellError;
    }
    if lower.contains("500") || lower.contains("internal server") || lower.contains("502") || lower.contains("503") {
        return ErrorCategory::ApiError;
    }

    ErrorCategory::Unknown
}

/// Extract a one-line summary from a verbose error.
fn extract_summary(error: &str) -> String {
    // Take the first non-empty line, capped at 120 chars
    let first_line = error
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(error)
        .trim();

    if first_line.len() <= 120 {
        first_line.to_string()
    } else {
        format!("{}...", &first_line[..117])
    }
}

/// Extract key details from verbose error output.
fn extract_key_details(error: &str) -> Vec<String> {
    let mut details = Vec::new();
    let lines: Vec<&str> = error.lines().collect();

    // Extract status codes
    for line in &lines {
        let lower = line.to_lowercase();
        if lower.contains("status") && (lower.contains("code") || lower.contains(": ")) {
            let trimmed = line.trim();
            if trimmed.len() <= 100 && !details.contains(&trimmed.to_string()) {
                details.push(trimmed.to_string());
            }
        }
    }

    // Extract file paths mentioned in errors
    for line in &lines {
        if (line.contains('/') || line.contains('\\')) && line.contains("error") {
            let trimmed = line.trim();
            if trimmed.len() <= 100 && details.len() < 3 {
                details.push(trimmed.to_string());
            }
        }
    }

    // Cap at 3 details
    details.truncate(3);
    details
}

/// Suggest a recovery action based on error category.
fn suggest_recovery(category: &ErrorCategory, tool_name: Option<&str>, _error: &str) -> Option<String> {
    match category {
        ErrorCategory::Timeout => Some("Retry with a shorter timeout or simpler request".to_string()),
        ErrorCategory::RateLimit => Some("Wait before retrying; reduce request frequency".to_string()),
        ErrorCategory::Permission => Some("Check credentials or request elevated permissions".to_string()),
        ErrorCategory::NotFound => {
            if let Some(tool) = tool_name {
                if tool.contains("file") {
                    return Some("Verify the file path exists with file_list first".to_string());
                }
            }
            Some("Verify the resource exists before accessing it".to_string())
        }
        ErrorCategory::Network => Some("Check network connectivity; the service may be down".to_string()),
        ErrorCategory::Validation => Some("Review the input parameters and fix any invalid values".to_string()),
        ErrorCategory::ShellError => Some("Check command syntax; review exit code for details".to_string()),
        ErrorCategory::ApiError => Some("External service error; retry or use a different endpoint".to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_categorize_timeout() {
        assert_eq!(categorize_error("Connection timed out after 30s"), ErrorCategory::Timeout);
        assert_eq!(categorize_error("Request deadline exceeded"), ErrorCategory::Timeout);
    }

    #[test]
    fn test_categorize_rate_limit() {
        assert_eq!(categorize_error("HTTP 429 Too Many Requests"), ErrorCategory::RateLimit);
        assert_eq!(categorize_error("Rate limit exceeded, retry after 60s"), ErrorCategory::RateLimit);
    }

    #[test]
    fn test_categorize_permission() {
        assert_eq!(categorize_error("Permission denied: /etc/shadow"), ErrorCategory::Permission);
        assert_eq!(categorize_error("HTTP 403 Forbidden"), ErrorCategory::Permission);
    }

    #[test]
    fn test_categorize_not_found() {
        assert_eq!(categorize_error("File not found: /tmp/missing.txt"), ErrorCategory::NotFound);
        assert_eq!(categorize_error("HTTP 404"), ErrorCategory::NotFound);
    }

    #[test]
    fn test_categorize_network() {
        assert_eq!(categorize_error("Connection refused to localhost:8080"), ErrorCategory::Network);
    }

    #[test]
    fn test_categorize_shell() {
        assert_eq!(categorize_error("Command failed with exit code 1"), ErrorCategory::ShellError);
    }

    #[test]
    fn test_categorize_unknown() {
        assert_eq!(categorize_error("Something went wrong"), ErrorCategory::Unknown);
    }

    #[test]
    fn test_extract_summary_short() {
        let summary = extract_summary("File not found");
        assert_eq!(summary, "File not found");
    }

    #[test]
    fn test_extract_summary_long() {
        let long = "x".repeat(200);
        let summary = extract_summary(&long);
        assert!(summary.len() <= 120);
        assert!(summary.ends_with("..."));
    }

    #[test]
    fn test_compact_error_full() {
        let error = compact_error(Some("web_fetch"), "HTTP 429 Too Many Requests\nRetry-After: 60\nPlease slow down.");
        assert_eq!(error.category, ErrorCategory::RateLimit);
        assert!(error.suggestion.is_some());
        assert_eq!(error.tool_name.as_deref(), Some("web_fetch"));
    }

    #[test]
    fn test_format_compact() {
        let error = compact_error(Some("file_read"), "File not found: /tmp/data.csv");
        let formatted = format_compact_error(&error);
        assert!(formatted.contains("[file_read]"));
        assert!(formatted.contains("NotFound"));
        assert!(formatted.contains("Verify"));
    }

    #[test]
    fn test_compact_tool_error_short() {
        let result = compact_tool_error("Small error", 500);
        assert_eq!(result, "Small error");
    }

    #[test]
    fn test_compact_tool_error_long() {
        let long = format!("Permission denied: {}", "x".repeat(1000));
        let result = compact_tool_error(&long, 200);
        assert!(result.len() <= 250); // some overhead from formatting
    }

    #[test]
    fn test_suggestion_file_not_found() {
        let suggestion = suggest_recovery(&ErrorCategory::NotFound, Some("file_read"), "");
        assert!(suggestion.unwrap().contains("file_list"));
    }

    #[test]
    fn test_suggestion_generic_not_found() {
        let suggestion = suggest_recovery(&ErrorCategory::NotFound, Some("web_fetch"), "");
        assert!(suggestion.unwrap().contains("Verify"));
    }

    #[test]
    fn test_serde_roundtrip() {
        let error = compact_error(None, "Connection timed out");
        let json = serde_json::to_string(&error).unwrap();
        let parsed: CompactedError = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.category, ErrorCategory::Timeout);
    }
}
