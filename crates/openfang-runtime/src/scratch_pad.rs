//! Ephemeral scratch-pads — per-turn working memory.
//!
//! Blueprint Factor 1: Each agent loop invocation gets a lightweight scratch-pad
//! that survives across tool calls within a single turn but is discarded when
//! the turn ends. This lets agents accumulate intermediate results, track
//! hypotheses, and store temporary notes without polluting persistent memory.
//!
//! The scratch-pad is injected into the system prompt as a "Working Memory"
//! section, updated after each tool call, and cleared on turn exit.

use std::collections::HashMap;
use std::time::Instant;

/// Maximum total characters across all scratch-pad entries.
const MAX_SCRATCH_CHARS: usize = 4000;

/// Maximum number of entries in the scratch-pad.
const MAX_ENTRIES: usize = 20;

/// Ephemeral working memory for a single agent loop turn.
#[derive(Debug, Clone)]
pub struct ScratchPad {
    /// Key-value entries (ordered by insertion).
    entries: Vec<ScratchEntry>,
    /// Total character count across all values.
    total_chars: usize,
    /// When this pad was created.
    created_at: Instant,
}

/// A single scratch-pad entry.
#[derive(Debug, Clone)]
pub struct ScratchEntry {
    /// Short label (e.g. "hypothesis", "url_list", "step_3_result").
    pub key: String,
    /// Free-form content.
    pub value: String,
    /// How many times this entry has been read/referenced.
    pub access_count: u32,
}

impl ScratchPad {
    /// Create a new empty scratch-pad for this turn.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            total_chars: 0,
            created_at: Instant::now(),
        }
    }

    /// Write or overwrite an entry.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();

        // Remove existing entry with same key
        if let Some(pos) = self.entries.iter().position(|e| e.key == key) {
            self.total_chars -= self.entries[pos].value.len();
            self.entries.remove(pos);
        }

        // Evict oldest if at capacity
        while self.entries.len() >= MAX_ENTRIES {
            let removed = self.entries.remove(0);
            self.total_chars -= removed.value.len();
        }

        // Truncate value if total would exceed budget
        let available = MAX_SCRATCH_CHARS.saturating_sub(self.total_chars);
        let final_value = if value.len() > available {
            value.chars().take(available).collect()
        } else {
            value
        };

        self.total_chars += final_value.len();
        self.entries.push(ScratchEntry {
            key,
            value: final_value,
            access_count: 0,
        });
    }

    /// Read an entry by key.
    pub fn get(&mut self, key: &str) -> Option<&str> {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.key == key) {
            entry.access_count += 1;
            Some(&entry.value)
        } else {
            None
        }
    }

    /// Remove an entry by key.
    pub fn remove(&mut self, key: &str) -> bool {
        if let Some(pos) = self.entries.iter().position(|e| e.key == key) {
            self.total_chars -= self.entries[pos].value.len();
            self.entries.remove(pos);
            true
        } else {
            false
        }
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the pad is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Total characters stored.
    pub fn total_chars(&self) -> usize {
        self.total_chars
    }

    /// Elapsed time since the pad was created.
    pub fn elapsed(&self) -> std::time::Duration {
        self.created_at.elapsed()
    }

    /// Render the scratch-pad as a markdown section for injection into prompts.
    /// Returns None if the pad is empty.
    pub fn to_prompt_section(&self) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let mut out = String::from("## Working Memory (this turn only)\n");
        for entry in &self.entries {
            out.push_str(&format!("- **{}**: {}\n", entry.key, entry.value));
        }
        out.push_str("\n*This working memory is ephemeral and will be cleared after this turn.*\n");
        Some(out)
    }

    /// Clear all entries (called when the turn ends).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.total_chars = 0;
    }

    /// Get all entries as key-value pairs (for serialization/debugging).
    pub fn entries(&self) -> Vec<(&str, &str)> {
        self.entries
            .iter()
            .map(|e| (e.key.as_str(), e.value.as_str()))
            .collect()
    }
}

impl Default for ScratchPad {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse scratch-pad commands from an agent's tool output.
///
/// Agents can use special markers in tool results to manipulate the scratch-pad:
/// - `[SCRATCH:SET key=value]` — write an entry
/// - `[SCRATCH:GET key]` — read an entry (returns in next prompt)
/// - `[SCRATCH:DEL key]` — delete an entry
///
/// Returns the commands found and the cleaned content with markers stripped.
pub fn parse_scratch_commands(content: &str) -> (Vec<ScratchCommand>, String) {
    let mut commands = Vec::new();
    let mut cleaned = String::with_capacity(content.len());

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("[SCRATCH:SET ") {
            if let Some(end) = rest.strip_suffix(']') {
                if let Some(eq_pos) = end.find('=') {
                    let key = end[..eq_pos].trim().to_string();
                    let value = end[eq_pos + 1..].trim().to_string();
                    commands.push(ScratchCommand::Set { key, value });
                    continue;
                }
            }
        }
        if let Some(rest) = trimmed.strip_prefix("[SCRATCH:GET ") {
            if let Some(key) = rest.strip_suffix(']') {
                commands.push(ScratchCommand::Get {
                    key: key.trim().to_string(),
                });
                continue;
            }
        }
        if let Some(rest) = trimmed.strip_prefix("[SCRATCH:DEL ") {
            if let Some(key) = rest.strip_suffix(']') {
                commands.push(ScratchCommand::Del {
                    key: key.trim().to_string(),
                });
                continue;
            }
        }
        cleaned.push_str(line);
        cleaned.push('\n');
    }

    // Trim trailing newline
    if cleaned.ends_with('\n') {
        cleaned.pop();
    }

    (commands, cleaned)
}

/// A command to manipulate the scratch-pad.
#[derive(Debug, Clone, PartialEq)]
pub enum ScratchCommand {
    Set { key: String, value: String },
    Get { key: String },
    Del { key: String },
}

/// Apply a list of scratch commands to a pad.
pub fn apply_commands(pad: &mut ScratchPad, commands: &[ScratchCommand]) -> HashMap<String, String> {
    let mut reads = HashMap::new();
    for cmd in commands {
        match cmd {
            ScratchCommand::Set { key, value } => {
                pad.set(key, value);
            }
            ScratchCommand::Get { key } => {
                if let Some(val) = pad.get(key) {
                    reads.insert(key.clone(), val.to_string());
                }
            }
            ScratchCommand::Del { key } => {
                pad.remove(key);
            }
        }
    }
    reads
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_pad_is_empty() {
        let pad = ScratchPad::new();
        assert!(pad.is_empty());
        assert_eq!(pad.len(), 0);
        assert_eq!(pad.total_chars(), 0);
    }

    #[test]
    fn test_set_and_get() {
        let mut pad = ScratchPad::new();
        pad.set("url", "https://example.com");
        assert_eq!(pad.get("url"), Some("https://example.com"));
        assert_eq!(pad.len(), 1);
    }

    #[test]
    fn test_overwrite() {
        let mut pad = ScratchPad::new();
        pad.set("count", "1");
        pad.set("count", "2");
        assert_eq!(pad.get("count"), Some("2"));
        assert_eq!(pad.len(), 1);
    }

    #[test]
    fn test_remove() {
        let mut pad = ScratchPad::new();
        pad.set("tmp", "data");
        assert!(pad.remove("tmp"));
        assert!(pad.is_empty());
        assert!(!pad.remove("nonexistent"));
    }

    #[test]
    fn test_clear() {
        let mut pad = ScratchPad::new();
        pad.set("a", "1");
        pad.set("b", "2");
        pad.clear();
        assert!(pad.is_empty());
        assert_eq!(pad.total_chars(), 0);
    }

    #[test]
    fn test_max_entries_eviction() {
        let mut pad = ScratchPad::new();
        for i in 0..25 {
            pad.set(format!("k{i}"), "v");
        }
        assert!(pad.len() <= MAX_ENTRIES);
        // First entries should have been evicted
        assert!(pad.get("k0").is_none());
    }

    #[test]
    fn test_char_budget_truncation() {
        let mut pad = ScratchPad::new();
        let big = "x".repeat(MAX_SCRATCH_CHARS + 100);
        pad.set("big", big);
        assert!(pad.total_chars() <= MAX_SCRATCH_CHARS);
    }

    #[test]
    fn test_prompt_section_empty() {
        let pad = ScratchPad::new();
        assert!(pad.to_prompt_section().is_none());
    }

    #[test]
    fn test_prompt_section_content() {
        let mut pad = ScratchPad::new();
        pad.set("hypothesis", "The bug is in the parser");
        let section = pad.to_prompt_section().unwrap();
        assert!(section.contains("Working Memory"));
        assert!(section.contains("hypothesis"));
        assert!(section.contains("The bug is in the parser"));
        assert!(section.contains("ephemeral"));
    }

    #[test]
    fn test_parse_scratch_commands() {
        let content = "Some output\n[SCRATCH:SET url=https://example.com]\nMore output\n[SCRATCH:GET url]\n[SCRATCH:DEL tmp]";
        let (cmds, cleaned) = parse_scratch_commands(content);
        assert_eq!(cmds.len(), 3);
        assert_eq!(
            cmds[0],
            ScratchCommand::Set {
                key: "url".into(),
                value: "https://example.com".into()
            }
        );
        assert_eq!(cmds[1], ScratchCommand::Get { key: "url".into() });
        assert_eq!(cmds[2], ScratchCommand::Del { key: "tmp".into() });
        assert!(cleaned.contains("Some output"));
        assert!(cleaned.contains("More output"));
        assert!(!cleaned.contains("[SCRATCH:"));
    }

    #[test]
    fn test_apply_commands() {
        let mut pad = ScratchPad::new();
        pad.set("existing", "hello");
        let cmds = vec![
            ScratchCommand::Set {
                key: "new".into(),
                value: "world".into(),
            },
            ScratchCommand::Get {
                key: "existing".into(),
            },
            ScratchCommand::Del {
                key: "existing".into(),
            },
        ];
        let reads = apply_commands(&mut pad, &cmds);
        assert_eq!(reads.get("existing").map(|s| s.as_str()), Some("hello"));
        assert_eq!(pad.get("new"), Some("world"));
        assert!(pad.get("existing").is_none());
    }

    #[test]
    fn test_access_count_increments() {
        let mut pad = ScratchPad::new();
        pad.set("key", "val");
        let _ = pad.get("key");
        let _ = pad.get("key");
        let entry = pad.entries.iter().find(|e| e.key == "key").unwrap();
        assert_eq!(entry.access_count, 2);
    }

    #[test]
    fn test_entries_view() {
        let mut pad = ScratchPad::new();
        pad.set("a", "1");
        pad.set("b", "2");
        let entries = pad.entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], ("a", "1"));
        assert_eq!(entries[1], ("b", "2"));
    }
}
