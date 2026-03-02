//! Memory consolidation and decay logic.
//!
//! Reduces confidence of old, unaccessed memories, boosts frequently-accessed
//! memories, and merges duplicate/similar memories within each agent namespace.

use crate::semantic::{cosine_similarity, embedding_from_bytes};
use chrono::Utc;
use openfang_types::error::{OpenFangError, OpenFangResult};
use openfang_types::memory::ConsolidationReport;
use rusqlite::Connection;
use std::sync::{Arc, Mutex};
use tracing::debug;

/// Scope-specific decay multipliers applied to the base decay rate.
/// Different memory types decay at different rates:
/// - Episodic (conversation memories): full decay rate
/// - Semantic (learned facts, cross-agent insights): 30% of base rate
/// - Procedural (how-to knowledge, self-corrections): 5% of base rate
/// - System/config memories: never decay
const SCOPE_DECAY_MULTIPLIERS: &[(&str, f64)] = &[
    ("episodic", 1.0),
    ("semantic", 0.3),
    ("procedural", 0.05),
    ("system", 0.0),
    ("shared", 0.3),
];

/// Memory consolidation engine.
#[derive(Clone)]
pub struct ConsolidationEngine {
    conn: Arc<Mutex<Connection>>,
    /// Decay rate: how much to reduce confidence per consolidation cycle.
    decay_rate: f32,
}

impl ConsolidationEngine {
    /// Create a new consolidation engine.
    pub fn new(conn: Arc<Mutex<Connection>>, decay_rate: f32) -> Self {
        Self { conn, decay_rate }
    }

    /// Run a full consolidation cycle: decay, boost, and merge.
    pub fn consolidate(&self) -> OpenFangResult<ConsolidationReport> {
        let start = std::time::Instant::now();
        let conn = self
            .conn
            .lock()
            .map_err(|e| OpenFangError::Internal(e.to_string()))?;

        // Stage 1: Decay confidence per scope — different memory types decay at different rates
        let cutoff = (Utc::now() - chrono::Duration::days(7)).to_rfc3339();
        let base_decay = self.decay_rate as f64;

        let mut decayed = 0usize;
        for &(scope, multiplier) in SCOPE_DECAY_MULTIPLIERS {
            if multiplier == 0.0 {
                continue; // Never decay this scope
            }
            let scope_decay_factor = 1.0 - (base_decay * multiplier);
            decayed += conn
                .execute(
                    "UPDATE memories SET confidence = MAX(0.1, confidence * ?1)
                     WHERE deleted = 0 AND accessed_at < ?2 AND confidence > 0.1 AND scope = ?3",
                    rusqlite::params![scope_decay_factor, cutoff, scope],
                )
                .map_err(|e| OpenFangError::Memory(e.to_string()))?;
        }
        // Fallback: decay unknown scopes at full rate
        let known_scopes: Vec<&str> = SCOPE_DECAY_MULTIPLIERS.iter().map(|(s, _)| *s).collect();
        let placeholders: String = known_scopes.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let full_decay_factor = 1.0 - base_decay;
        let mut fallback_params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        fallback_params.push(Box::new(full_decay_factor));
        fallback_params.push(Box::new(cutoff.clone()));
        for s in &known_scopes {
            fallback_params.push(Box::new(s.to_string()));
        }
        let fallback_sql = format!(
            "UPDATE memories SET confidence = MAX(0.1, confidence * ?1)
             WHERE deleted = 0 AND accessed_at < ?2 AND confidence > 0.1 AND scope NOT IN ({placeholders})"
        );
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            fallback_params.iter().map(|p| p.as_ref()).collect();
        decayed += conn
            .execute(&fallback_sql, param_refs.as_slice())
            .map_err(|e| OpenFangError::Memory(e.to_string()))?;

        // Stage 2: Access-count confidence boosting
        // Memories accessed >= 10 times get a floor of 0.7
        let boosted_high = conn
            .execute(
                "UPDATE memories SET confidence = MAX(confidence, 0.7)
                 WHERE deleted = 0 AND access_count >= 10 AND confidence < 0.7",
                [],
            )
            .map_err(|e| OpenFangError::Memory(e.to_string()))?;

        // Memories accessed >= 5 times get a floor of 0.5
        let boosted_mid = conn
            .execute(
                "UPDATE memories SET confidence = MAX(confidence, 0.5)
                 WHERE deleted = 0 AND access_count >= 5 AND access_count < 10 AND confidence < 0.5",
                [],
            )
            .map_err(|e| OpenFangError::Memory(e.to_string()))?;

        let total_boosted = (boosted_high + boosted_mid) as u64;

        // Stage 3: Merge duplicates within each agent namespace
        let merged = self.merge_duplicates(&conn)?;

        let duration_ms = start.elapsed().as_millis() as u64;

        debug!(
            decayed,
            boosted = total_boosted,
            merged,
            duration_ms,
            "Consolidation cycle complete"
        );

        Ok(ConsolidationReport {
            memories_merged: merged,
            memories_decayed: decayed as u64,
            memories_boosted: total_boosted,
            duration_ms,
            memories_synthesized: 0,
        })
    }

    /// Merge duplicate memories within each agent namespace.
    /// Compares embeddings pairwise; if cosine similarity > 0.85, keeps the one
    /// with the higher access_count and soft-deletes the other.
    fn merge_duplicates(&self, conn: &Connection) -> OpenFangResult<u64> {
        // Fetch all non-deleted memories that have embeddings, grouped by agent_id
        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, access_count, embedding
                 FROM memories
                 WHERE deleted = 0 AND embedding IS NOT NULL
                 ORDER BY agent_id, access_count DESC",
            )
            .map_err(|e| OpenFangError::Memory(e.to_string()))?;

        let rows: Vec<(String, String, i64, Vec<u8>)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            })
            .map_err(|e| OpenFangError::Memory(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        // Group by agent_id
        let mut agent_groups: std::collections::HashMap<String, Vec<(String, i64, Vec<f32>)>> =
            std::collections::HashMap::new();
        for (id, agent_id, access_count, emb_bytes) in &rows {
            let emb = embedding_from_bytes(emb_bytes);
            if !emb.is_empty() {
                agent_groups
                    .entry(agent_id.clone())
                    .or_default()
                    .push((id.clone(), *access_count, emb));
            }
        }

        let mut total_merged = 0u64;
        let mut ids_to_delete: Vec<String> = Vec::new();

        for memories in agent_groups.values() {
            // Pairwise comparison within agent namespace (O(n^2) — fine for <10K per agent)
            let len = memories.len();
            let mut deleted_indices: std::collections::HashSet<usize> = Default::default();

            for i in 0..len {
                if deleted_indices.contains(&i) {
                    continue;
                }
                for j in (i + 1)..len {
                    if deleted_indices.contains(&j) {
                        continue;
                    }
                    let sim = cosine_similarity(&memories[i].2, &memories[j].2);
                    if sim > 0.85 {
                        // Keep the one with higher access_count (sorted desc, so i is keeper)
                        ids_to_delete.push(memories[j].0.clone());
                        deleted_indices.insert(j);
                        total_merged += 1;
                    }
                }
            }
        }

        // Soft-delete merged memories and boost keepers
        let now = Utc::now().to_rfc3339();
        for id in &ids_to_delete {
            if let Err(e) = conn.execute(
                "UPDATE memories SET deleted = 1 WHERE id = ?1",
                rusqlite::params![id],
            ) {
                tracing::warn!(id = %id, error = %e, "Failed to soft-delete merged memory");
            }
        }

        // Boost access_count for keepers that had duplicates merged
        if total_merged > 0 {
            debug!(
                merged = total_merged,
                "Merged duplicate memories within agent namespaces"
            );
        }

        // Touch the last-accessed timestamp on remaining non-deleted memories
        if !ids_to_delete.is_empty() {
            if let Err(e) = conn.execute(
                "UPDATE memories SET accessed_at = ?1 WHERE deleted = 0 AND embedding IS NOT NULL",
                rusqlite::params![now],
            ) {
                tracing::warn!(error = %e, "Failed to update accessed_at after merge");
            }
        }

        Ok(total_merged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration::run_migrations;

    fn setup() -> ConsolidationEngine {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        ConsolidationEngine::new(Arc::new(Mutex::new(conn)), 0.1)
    }

    #[test]
    fn test_consolidation_empty() {
        let engine = setup();
        let report = engine.consolidate().unwrap();
        assert_eq!(report.memories_decayed, 0);
        assert_eq!(report.memories_boosted, 0);
        assert_eq!(report.memories_merged, 0);
    }

    #[test]
    fn test_consolidation_decays_old_memories() {
        let engine = setup();
        let conn = engine.conn.lock().unwrap();
        // Insert an old memory
        let old_date = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        conn.execute(
            "INSERT INTO memories (id, agent_id, content, source, scope, confidence, metadata, created_at, accessed_at, access_count, deleted)
             VALUES ('test-id', 'agent-1', 'old memory', '\"conversation\"', 'episodic', 0.9, '{}', ?1, ?1, 0, 0)",
            rusqlite::params![old_date],
        ).unwrap();
        drop(conn);

        let report = engine.consolidate().unwrap();
        assert_eq!(report.memories_decayed, 1);

        // Verify confidence was reduced
        let conn = engine.conn.lock().unwrap();
        let confidence: f64 = conn
            .query_row(
                "SELECT confidence FROM memories WHERE id = 'test-id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(confidence < 0.9);
    }

    #[test]
    fn test_consolidation_boosts_high_access() {
        let engine = setup();
        let conn = engine.conn.lock().unwrap();
        let old_date = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        // Insert a frequently-accessed memory with low confidence
        conn.execute(
            "INSERT INTO memories (id, agent_id, content, source, scope, confidence, metadata, created_at, accessed_at, access_count, deleted)
             VALUES ('boost-test', 'agent-1', 'popular memory', '\"conversation\"', 'episodic', 0.3, '{}', ?1, ?1, 15, 0)",
            rusqlite::params![old_date],
        ).unwrap();
        drop(conn);

        let report = engine.consolidate().unwrap();
        assert!(report.memories_boosted >= 1);

        // Verify confidence was boosted to at least 0.7
        let conn = engine.conn.lock().unwrap();
        let confidence: f64 = conn
            .query_row(
                "SELECT confidence FROM memories WHERE id = 'boost-test'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(confidence >= 0.7);
    }

    #[test]
    fn test_scope_aware_decay_rates() {
        let engine = setup();
        let conn = engine.conn.lock().unwrap();
        let old_date = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();

        // Insert memories with different scopes, all at confidence 0.9
        for (id_suffix, scope) in &[("ep", "episodic"), ("sem", "semantic"), ("proc", "procedural"), ("sys", "system")] {
            conn.execute(
                "INSERT INTO memories (id, agent_id, content, source, scope, confidence, metadata, created_at, accessed_at, access_count, deleted)
                 VALUES (?1, 'agent-1', 'test', '\"conversation\"', ?2, 0.9, '{}', ?3, ?3, 0, 0)",
                rusqlite::params![format!("scope-{id_suffix}"), scope, old_date],
            ).unwrap();
        }
        drop(conn);

        engine.consolidate().unwrap();

        let conn = engine.conn.lock().unwrap();
        let get_conf = |id: &str| -> f64 {
            conn.query_row(
                "SELECT confidence FROM memories WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            ).unwrap()
        };

        let episodic_conf = get_conf("scope-ep");
        let semantic_conf = get_conf("scope-sem");
        let procedural_conf = get_conf("scope-proc");
        let system_conf = get_conf("scope-sys");

        // Episodic should decay the most
        assert!(episodic_conf < 0.9, "episodic should decay: {episodic_conf}");
        // Semantic decays less than episodic
        assert!(semantic_conf > episodic_conf, "semantic ({semantic_conf}) should decay less than episodic ({episodic_conf})");
        // Procedural decays even less
        assert!(procedural_conf > semantic_conf, "procedural ({procedural_conf}) should decay less than semantic ({semantic_conf})");
        // System never decays
        assert_eq!(system_conf, 0.9, "system should not decay");
    }

    #[test]
    fn test_consolidation_merges_duplicates() {
        let engine = setup();
        let conn = engine.conn.lock().unwrap();
        let now = Utc::now().to_rfc3339();

        // Create two memories with nearly identical embeddings
        let emb1 = [0.9f32, 0.1, 0.0, 0.0];
        let emb2 = [0.89f32, 0.11, 0.01, 0.0]; // very similar to emb1
        let emb1_bytes: Vec<u8> = emb1.iter().flat_map(|f| f.to_le_bytes()).collect();
        let emb2_bytes: Vec<u8> = emb2.iter().flat_map(|f| f.to_le_bytes()).collect();

        conn.execute(
            "INSERT INTO memories (id, agent_id, content, source, scope, confidence, metadata, created_at, accessed_at, access_count, deleted, embedding)
             VALUES ('dup-1', 'agent-1', 'Rust is great', '\"conversation\"', 'episodic', 0.9, '{}', ?1, ?1, 5, 0, ?2)",
            rusqlite::params![now, emb1_bytes],
        ).unwrap();
        conn.execute(
            "INSERT INTO memories (id, agent_id, content, source, scope, confidence, metadata, created_at, accessed_at, access_count, deleted, embedding)
             VALUES ('dup-2', 'agent-1', 'Rust is awesome', '\"conversation\"', 'episodic', 0.8, '{}', ?1, ?1, 2, 0, ?2)",
            rusqlite::params![now, emb2_bytes],
        ).unwrap();
        drop(conn);

        let report = engine.consolidate().unwrap();
        assert!(report.memories_merged >= 1);

        // Verify one was soft-deleted
        let conn = engine.conn.lock().unwrap();
        let active_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE agent_id = 'agent-1' AND deleted = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(active_count, 1);
    }
}
