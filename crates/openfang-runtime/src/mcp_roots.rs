//! MCP Roots — filesystem boundary declarations for MCP servers.
//!
//! Roots let MCP clients declare which filesystem paths an agent is allowed
//! to access, enabling servers to scope file operations. This improves
//! security (agents can't escape their workspace) and provides context
//! (servers know which project the agent is working in).
//!
//! MCP protocol methods:
//! - `roots/list` — server requests the client's root list
//! - `notifications/roots/list_changed` — client notifies roots updated

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A filesystem root declared by the MCP client.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpRoot {
    /// URI of the root (file:///path/to/dir).
    pub uri: String,
    /// Human-readable name for this root.
    #[serde(default)]
    pub name: Option<String>,
}

impl McpRoot {
    /// Create a root from a local filesystem path.
    pub fn from_path(path: &Path, name: Option<&str>) -> Self {
        let uri = format!("file://{}", path.display());
        Self {
            uri,
            name: name.map(|s| s.to_string()),
        }
    }

    /// Extract the filesystem path from the root URI.
    pub fn to_path(&self) -> Option<PathBuf> {
        self.uri
            .strip_prefix("file://")
            .map(PathBuf::from)
    }

    /// Check if a given path is within this root's boundary.
    pub fn contains(&self, path: &Path) -> bool {
        if let Some(root_path) = self.to_path() {
            // Canonicalize both paths to prevent traversal attacks
            let root_canonical = root_path.canonicalize().ok();
            let path_canonical = path.canonicalize().ok();

            match (root_canonical, path_canonical) {
                (Some(root), Some(target)) => target.starts_with(&root),
                // If canonicalization fails, do a prefix check on the raw paths
                _ => path.starts_with(&root_path),
            }
        } else {
            false
        }
    }
}

/// Root store — manages the set of declared filesystem roots.
#[derive(Debug, Clone, Default)]
pub struct RootStore {
    roots: Vec<McpRoot>,
}

impl RootStore {
    pub fn new() -> Self {
        Self { roots: Vec::new() }
    }

    /// Create a root store from a list of roots.
    pub fn from_roots(roots: Vec<McpRoot>) -> Self {
        Self { roots }
    }

    /// Add a root. Returns true if it was new (not a duplicate).
    pub fn add(&mut self, root: McpRoot) -> bool {
        if self.roots.iter().any(|r| r.uri == root.uri) {
            return false;
        }
        self.roots.push(root);
        true
    }

    /// Remove a root by URI.
    pub fn remove(&mut self, uri: &str) -> bool {
        let len = self.roots.len();
        self.roots.retain(|r| r.uri != uri);
        self.roots.len() < len
    }

    /// Get all roots.
    pub fn roots(&self) -> &[McpRoot] {
        &self.roots
    }

    /// Check if a path is within any declared root.
    pub fn is_allowed(&self, path: &Path) -> bool {
        if self.roots.is_empty() {
            return true; // No roots declared = unrestricted
        }
        self.roots.iter().any(|r| r.contains(path))
    }

    /// Build the JSON response for `roots/list`.
    pub fn to_list_response(&self) -> serde_json::Value {
        let roots: Vec<serde_json::Value> = self
            .roots
            .iter()
            .map(|r| {
                let mut obj = serde_json::json!({"uri": r.uri});
                if let Some(ref name) = r.name {
                    obj["name"] = serde_json::Value::String(name.clone());
                }
                obj
            })
            .collect();
        serde_json::json!({"roots": roots})
    }

    /// Build a `notifications/roots/list_changed` notification.
    pub fn build_change_notification(&self) -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/roots/list_changed",
        })
    }

    /// Create roots from an agent's workspace path.
    pub fn from_workspace(workspace: &Path) -> Self {
        let mut store = Self::new();
        store.add(McpRoot::from_path(
            workspace,
            Some("agent workspace"),
        ));
        store
    }

    /// Number of roots.
    pub fn len(&self) -> usize {
        self.roots.len()
    }

    /// Whether the root store is empty.
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_root_from_path() {
        let root = McpRoot::from_path(Path::new("/home/user/project"), Some("my project"));
        assert_eq!(root.uri, "file:///home/user/project");
        assert_eq!(root.name.as_deref(), Some("my project"));
    }

    #[test]
    fn test_root_to_path() {
        let root = McpRoot {
            uri: "file:///home/user/project".into(),
            name: None,
        };
        assert_eq!(root.to_path(), Some(PathBuf::from("/home/user/project")));
    }

    #[test]
    fn test_root_to_path_invalid() {
        let root = McpRoot {
            uri: "https://example.com".into(),
            name: None,
        };
        assert_eq!(root.to_path(), None);
    }

    #[test]
    fn test_root_contains() {
        let root = McpRoot::from_path(Path::new("/tmp"), None);
        // /tmp exists on Linux, so canonicalization should work
        assert!(root.contains(Path::new("/tmp")));
    }

    #[test]
    fn test_root_store_add_dedup() {
        let mut store = RootStore::new();
        let root = McpRoot::from_path(Path::new("/home/user"), None);
        assert!(store.add(root.clone()));
        assert!(!store.add(root)); // Duplicate
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn test_root_store_remove() {
        let mut store = RootStore::new();
        store.add(McpRoot::from_path(Path::new("/home/a"), None));
        store.add(McpRoot::from_path(Path::new("/home/b"), None));
        assert_eq!(store.len(), 2);
        assert!(store.remove("file:///home/a"));
        assert_eq!(store.len(), 1);
        assert!(!store.remove("file:///nonexistent"));
    }

    #[test]
    fn test_root_store_empty_allows_all() {
        let store = RootStore::new();
        assert!(store.is_allowed(Path::new("/any/path")));
    }

    #[test]
    fn test_root_store_list_response() {
        let mut store = RootStore::new();
        store.add(McpRoot::from_path(Path::new("/project"), Some("proj")));
        let resp = store.to_list_response();
        let roots = resp["roots"].as_array().unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0]["uri"], "file:///project");
        assert_eq!(roots[0]["name"], "proj");
    }

    #[test]
    fn test_root_store_change_notification() {
        let store = RootStore::new();
        let notif = store.build_change_notification();
        assert_eq!(notif["method"], "notifications/roots/list_changed");
        assert_eq!(notif["jsonrpc"], "2.0");
    }

    #[test]
    fn test_root_store_from_workspace() {
        let store = RootStore::from_workspace(Path::new("/home/agent/workspace"));
        assert_eq!(store.len(), 1);
        assert_eq!(store.roots()[0].name.as_deref(), Some("agent workspace"));
    }

    #[test]
    fn test_root_serde_roundtrip() {
        let root = McpRoot {
            uri: "file:///data/project".into(),
            name: Some("test".into()),
        };
        let json = serde_json::to_string(&root).unwrap();
        let back: McpRoot = serde_json::from_str(&json).unwrap();
        assert_eq!(root, back);
    }

    #[test]
    fn test_root_no_name() {
        let json = r#"{"uri": "file:///data"}"#;
        let root: McpRoot = serde_json::from_str(json).unwrap();
        assert!(root.name.is_none());
        assert_eq!(root.uri, "file:///data");
    }
}
