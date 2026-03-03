//! Compile-time embedded agent templates.
//!
//! Core bundled agent templates are embedded into the binary via `include_str!`.
//! This ensures `openfang agent new` works immediately after install — no filesystem
//! discovery needed.

/// Returns all bundled agent templates as `(name, toml_content)` pairs.
pub fn bundled_agents() -> Vec<(&'static str, &'static str)> {
    vec![
        ("architect", include_str!("../../../agents/architect/agent.toml")),
        ("assistant", include_str!("../../../agents/assistant/agent.toml")),
        ("engineer", include_str!("../../../agents/engineer/agent.toml")),
        ("job-hunter", include_str!("../../../agents/job-hunter/agent.toml")),
        ("orchestrator", include_str!("../../../agents/orchestrator/agent.toml")),
        ("planner", include_str!("../../../agents/planner/agent.toml")),
        ("researcher", include_str!("../../../agents/researcher/agent.toml")),
        ("security-auditor", include_str!("../../../agents/security-auditor/agent.toml")),
        ("social-media", include_str!("../../../agents/social-media/agent.toml")),
        ("sysadmin", include_str!("../../../agents/sysadmin/agent.toml")),
    ]
}

/// Install bundled agent templates to `~/.openfang/agents/`.
/// Skips any template that already exists on disk (user customization preserved).
pub fn install_bundled_agents(agents_dir: &std::path::Path) {
    for (name, content) in bundled_agents() {
        let dest_dir = agents_dir.join(name);
        let dest_file = dest_dir.join("agent.toml");
        if dest_file.exists() {
            continue; // Preserve user customization
        }
        if std::fs::create_dir_all(&dest_dir).is_ok() {
            let _ = std::fs::write(&dest_file, content);
        }
    }
}
