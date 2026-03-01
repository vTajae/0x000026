//! Knowledge graph backed by SQLite.
//!
//! Stores entities and relations with support for graph pattern queries.

use chrono::Utc;
use openfang_types::error::{OpenFangError, OpenFangResult};
use openfang_types::memory::{
    Entity, EntityType, GraphMatch, GraphPattern, Relation, RelationType,
};
use regex_lite::Regex;
use rusqlite::Connection;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// Knowledge graph store backed by SQLite.
#[derive(Clone)]
pub struct KnowledgeStore {
    conn: Arc<Mutex<Connection>>,
}

impl KnowledgeStore {
    /// Create a new knowledge store wrapping the given connection.
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    /// Add an entity to the knowledge graph.
    pub fn add_entity(&self, entity: Entity) -> OpenFangResult<String> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| OpenFangError::Internal(e.to_string()))?;
        let id = if entity.id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            entity.id.clone()
        };
        let entity_type_str = serde_json::to_string(&entity.entity_type)
            .map_err(|e| OpenFangError::Serialization(e.to_string()))?;
        let props_str = serde_json::to_string(&entity.properties)
            .map_err(|e| OpenFangError::Serialization(e.to_string()))?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO entities (id, entity_type, name, properties, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(id) DO UPDATE SET name = ?3, properties = ?4, updated_at = ?5",
            rusqlite::params![id, entity_type_str, entity.name, props_str, now],
        )
        .map_err(|e| OpenFangError::Memory(e.to_string()))?;
        Ok(id)
    }

    /// Add a relation between two entities.
    pub fn add_relation(&self, relation: Relation) -> OpenFangResult<String> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| OpenFangError::Internal(e.to_string()))?;
        let id = Uuid::new_v4().to_string();
        let rel_type_str = serde_json::to_string(&relation.relation)
            .map_err(|e| OpenFangError::Serialization(e.to_string()))?;
        let props_str = serde_json::to_string(&relation.properties)
            .map_err(|e| OpenFangError::Serialization(e.to_string()))?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO relations (id, source_entity, relation_type, target_entity, properties, confidence, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                id,
                relation.source,
                rel_type_str,
                relation.target,
                props_str,
                relation.confidence as f64,
                now,
            ],
        )
        .map_err(|e| OpenFangError::Memory(e.to_string()))?;
        Ok(id)
    }

    /// Query the knowledge graph with a pattern.
    pub fn query_graph(&self, pattern: GraphPattern) -> OpenFangResult<Vec<GraphMatch>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| OpenFangError::Internal(e.to_string()))?;

        let mut sql = String::from(
            "SELECT
                s.id, s.entity_type, s.name, s.properties, s.created_at, s.updated_at,
                r.id, r.source_entity, r.relation_type, r.target_entity, r.properties, r.confidence, r.created_at,
                t.id, t.entity_type, t.name, t.properties, t.created_at, t.updated_at
             FROM relations r
             JOIN entities s ON r.source_entity = s.id
             JOIN entities t ON r.target_entity = t.id
             WHERE 1=1",
        );
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut idx = 1;

        if let Some(ref source) = pattern.source {
            sql.push_str(&format!(" AND (s.id = ?{idx} OR s.name = ?{idx})"));
            params.push(Box::new(source.clone()));
            idx += 1;
        }
        if let Some(ref relation) = pattern.relation {
            let rel_str = serde_json::to_string(relation)
                .map_err(|e| OpenFangError::Serialization(e.to_string()))?;
            sql.push_str(&format!(" AND r.relation_type = ?{idx}"));
            params.push(Box::new(rel_str));
            idx += 1;
        }
        if let Some(ref target) = pattern.target {
            sql.push_str(&format!(" AND (t.id = ?{idx} OR t.name = ?{idx})"));
            params.push(Box::new(target.clone()));
            let _ = idx;
        }

        sql.push_str(" LIMIT 100");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| OpenFangError::Memory(e.to_string()))?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok(RawGraphRow {
                    s_id: row.get(0)?,
                    s_type: row.get(1)?,
                    s_name: row.get(2)?,
                    s_props: row.get(3)?,
                    s_created: row.get(4)?,
                    s_updated: row.get(5)?,
                    r_id: row.get(6)?,
                    r_source: row.get(7)?,
                    r_type: row.get(8)?,
                    r_target: row.get(9)?,
                    r_props: row.get(10)?,
                    r_confidence: row.get(11)?,
                    r_created: row.get(12)?,
                    t_id: row.get(13)?,
                    t_type: row.get(14)?,
                    t_name: row.get(15)?,
                    t_props: row.get(16)?,
                    t_created: row.get(17)?,
                    t_updated: row.get(18)?,
                })
            })
            .map_err(|e| OpenFangError::Memory(e.to_string()))?;

        let mut matches = Vec::new();
        for row_result in rows {
            let r = row_result.map_err(|e| OpenFangError::Memory(e.to_string()))?;
            matches.push(GraphMatch {
                source: parse_entity(
                    &r.s_id,
                    &r.s_type,
                    &r.s_name,
                    &r.s_props,
                    &r.s_created,
                    &r.s_updated,
                ),
                relation: parse_relation(
                    &r.r_source,
                    &r.r_type,
                    &r.r_target,
                    &r.r_props,
                    r.r_confidence,
                    &r.r_created,
                ),
                target: parse_entity(
                    &r.t_id,
                    &r.t_type,
                    &r.t_name,
                    &r.t_props,
                    &r.t_created,
                    &r.t_updated,
                ),
            });
        }
        Ok(matches)
    }

    /// Find or create an entity by (name, entity_type).
    /// If found, updates the timestamp and returns the existing ID.
    /// If not found, creates a new entity.
    pub fn find_or_create_entity(&self, entity: Entity) -> OpenFangResult<String> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| OpenFangError::Internal(e.to_string()))?;
        let entity_type_str = serde_json::to_string(&entity.entity_type)
            .map_err(|e| OpenFangError::Serialization(e.to_string()))?;

        // Look up by (name, entity_type)
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM entities WHERE name = ?1 AND entity_type = ?2",
                rusqlite::params![entity.name, entity_type_str],
                |row| row.get(0),
            )
            .ok();

        if let Some(id) = existing {
            // Update timestamp on existing entity
            let now = Utc::now().to_rfc3339();
            let _ = conn.execute(
                "UPDATE entities SET updated_at = ?1 WHERE id = ?2",
                rusqlite::params![now, id],
            );
            Ok(id)
        } else {
            // Create new entity
            drop(conn);
            self.add_entity(entity)
        }
    }

    /// Export all entities from the knowledge graph.
    pub fn export_entities(&self) -> OpenFangResult<Vec<Entity>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| OpenFangError::Internal(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT id, entity_type, name, properties, created_at, updated_at FROM entities ORDER BY created_at")
            .map_err(|e| OpenFangError::Memory(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .map_err(|e| OpenFangError::Memory(e.to_string()))?;
        let mut entities = Vec::new();
        for r in rows {
            let (id, etype, name, props, created, updated) =
                r.map_err(|e| OpenFangError::Memory(e.to_string()))?;
            entities.push(parse_entity(&id, &etype, &name, &props, &created, &updated));
        }
        Ok(entities)
    }

    /// Export all relations from the knowledge graph.
    pub fn export_relations(&self) -> OpenFangResult<Vec<Relation>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| OpenFangError::Internal(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT source_entity, relation_type, target_entity, properties, confidence, created_at FROM relations ORDER BY created_at")
            .map_err(|e| OpenFangError::Memory(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, f64>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .map_err(|e| OpenFangError::Memory(e.to_string()))?;
        let mut relations = Vec::new();
        for r in rows {
            let (source, rtype, target, props, conf, created) =
                r.map_err(|e| OpenFangError::Memory(e.to_string()))?;
            relations.push(parse_relation(&source, &rtype, &target, &props, conf, &created));
        }
        Ok(relations)
    }

    /// Import an entity (upsert by name + entity_type).
    pub fn import_entity(&self, entity: &Entity) -> OpenFangResult<()> {
        let _ = self.find_or_create_entity(entity.clone())?;
        Ok(())
    }

    /// Import a relation (upsert by source + relation + target).
    pub fn import_relation(&self, relation: &Relation) -> OpenFangResult<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| OpenFangError::Internal(e.to_string()))?;
        let rel_type_str = serde_json::to_string(&relation.relation)
            .map_err(|e| OpenFangError::Serialization(e.to_string()))?;
        // Check for existing
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM relations WHERE source_entity = ?1 AND relation_type = ?2 AND target_entity = ?3",
                rusqlite::params![relation.source, rel_type_str, relation.target],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .unwrap_or(false);
        if exists {
            return Ok(());
        }
        drop(conn);
        let _ = self.add_relation(relation.clone())?;
        Ok(())
    }

    /// Find entities whose names appear in the given text (case-insensitive substring match).
    /// Returns up to 10 matching entities.
    pub fn find_entities_in_text(&self, text: &str) -> OpenFangResult<Vec<Entity>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| OpenFangError::Internal(e.to_string()))?;

        let text_lower = text.to_lowercase();

        let mut stmt = conn
            .prepare(
                "SELECT id, entity_type, name, properties, created_at, updated_at
                 FROM entities
                 WHERE LENGTH(name) >= 2
                 ORDER BY updated_at DESC
                 LIMIT 100",
            )
            .map_err(|e| OpenFangError::Memory(e.to_string()))?;

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .map_err(|e| OpenFangError::Memory(e.to_string()))?;

        let mut entities = Vec::new();
        for row_result in rows {
            let (id, etype, name, props, created, updated) =
                row_result.map_err(|e| OpenFangError::Memory(e.to_string()))?;

            // Check if entity name appears in text (case-insensitive)
            if text_lower.contains(&name.to_lowercase()) {
                entities.push(parse_entity(&id, &etype, &name, &props, &created, &updated));
                if entities.len() >= 10 {
                    break;
                }
            }
        }

        Ok(entities)
    }
}

/// An extracted fact from text (subject-relation-object triple).
#[derive(Debug, Clone)]
pub struct ExtractedFact {
    pub subject_name: String,
    pub subject_type: EntityType,
    pub relation: RelationType,
    pub object_name: String,
    pub object_type: EntityType,
    pub confidence: f32,
}

/// Extract facts from text using pattern matching.
/// No LLM call — pure regex, zero latency/cost.
pub fn extract_facts_from_text(text: &str) -> Vec<ExtractedFact> {
    let mut facts = Vec::new();

    // Name capture: uppercase letter followed by lowercase letters, optionally
    // followed by more capitalized words. NO (?i) flag — [A-Z] must be literal
    // uppercase to avoid capturing common words like "and", "the", "is".
    const NAME: &str = r"([A-Z][a-z]+(?:\s+[A-Z][a-z]+)*)";

    // Pattern: "X works at Y" / "X work at Y" / "X is working at Y"
    let works_at = Regex::new(&format!(
        r"\b{NAME}\s+(?:[Ww]orks?|[Ii]s [Ww]orking|[Aa]m [Ww]orking)\s+[Aa]t\s+{NAME}"
    )).unwrap();
    for cap in works_at.captures_iter(text) {
        facts.push(ExtractedFact {
            subject_name: cap[1].to_string(),
            subject_type: EntityType::Person,
            relation: RelationType::WorksAt,
            object_name: cap[2].to_string(),
            object_type: EntityType::Organization,
            confidence: 0.7,
        });
    }

    // Pattern: "I work at Y" / "I'm working at Y"
    let i_work_at = Regex::new(&format!(
        r"\b[Ii]\s+(?:[Ww]ork|[Aa]m [Ww]orking|'m [Ww]orking)\s+[Aa]t\s+{NAME}"
    )).unwrap();
    for cap in i_work_at.captures_iter(text) {
        facts.push(ExtractedFact {
            subject_name: "User".to_string(),
            subject_type: EntityType::Person,
            relation: RelationType::WorksAt,
            object_name: cap[1].to_string(),
            object_type: EntityType::Organization,
            confidence: 0.7,
        });
    }

    // Pattern: "X is located in Y" / "X is based in Y"
    let located_in = Regex::new(&format!(
        r"\b{NAME}\s+(?:[Ii]s\s+)?(?:[Ll]ocated|[Bb]ased)\s+[Ii]n\s+{NAME}"
    )).unwrap();
    for cap in located_in.captures_iter(text) {
        facts.push(ExtractedFact {
            subject_name: cap[1].to_string(),
            subject_type: EntityType::Organization,
            relation: RelationType::LocatedIn,
            object_name: cap[2].to_string(),
            object_type: EntityType::Location,
            confidence: 0.7,
        });
    }

    // Pattern: "I'm from Y" / "I am from Y"
    let from_place = Regex::new(&format!(
        r"\b(?:[Ii]'m|[Ii] [Aa]m)\s+[Ff]rom\s+{NAME}"
    )).unwrap();
    for cap in from_place.captures_iter(text) {
        facts.push(ExtractedFact {
            subject_name: "User".to_string(),
            subject_type: EntityType::Person,
            relation: RelationType::LocatedIn,
            object_name: cap[1].to_string(),
            object_type: EntityType::Location,
            confidence: 0.7,
        });
    }

    // Pattern: "X uses Y" / "X is using Y"
    let uses = Regex::new(&format!(
        r"\b{NAME}\s+(?:[Uu]ses?|[Ii]s [Uu]sing)\s+{NAME}"
    )).unwrap();
    for cap in uses.captures_iter(text) {
        facts.push(ExtractedFact {
            subject_name: cap[1].to_string(),
            subject_type: EntityType::Person,
            relation: RelationType::Uses,
            object_name: cap[2].to_string(),
            object_type: EntityType::Tool,
            confidence: 0.7,
        });
    }

    // Pattern: "X depends on Y"
    let depends = Regex::new(&format!(
        r"\b{NAME}\s+[Dd]epends?\s+[Oo]n\s+{NAME}"
    )).unwrap();
    for cap in depends.captures_iter(text) {
        facts.push(ExtractedFact {
            subject_name: cap[1].to_string(),
            subject_type: EntityType::Project,
            relation: RelationType::DependsOn,
            object_name: cap[2].to_string(),
            object_type: EntityType::Project,
            confidence: 0.7,
        });
    }

    // Pattern: "my name is X" / "My name is X"
    let my_name = Regex::new(&format!(
        r"\b[Mm]y\s+[Nn]ame\s+[Ii]s\s+{NAME}"
    )).unwrap();
    for cap in my_name.captures_iter(text) {
        facts.push(ExtractedFact {
            subject_name: "User".to_string(),
            subject_type: EntityType::Person,
            relation: RelationType::Custom("named".to_string()),
            object_name: cap[1].to_string(),
            object_type: EntityType::Person,
            confidence: 0.8,
        });
    }

    // Pattern: "X prefers Y"
    let prefers = Regex::new(&format!(
        r"\b{NAME}\s+[Pp]refers?\s+{NAME}"
    )).unwrap();
    for cap in prefers.captures_iter(text) {
        facts.push(ExtractedFact {
            subject_name: cap[1].to_string(),
            subject_type: EntityType::Person,
            relation: RelationType::Custom("prefers".to_string()),
            object_name: cap[2].to_string(),
            object_type: EntityType::Concept,
            confidence: 0.7,
        });
    }

    facts
}

/// Raw row from a graph query.
struct RawGraphRow {
    s_id: String,
    s_type: String,
    s_name: String,
    s_props: String,
    s_created: String,
    s_updated: String,
    r_id: String,
    r_source: String,
    r_type: String,
    r_target: String,
    r_props: String,
    r_confidence: f64,
    r_created: String,
    t_id: String,
    t_type: String,
    t_name: String,
    t_props: String,
    t_created: String,
    t_updated: String,
}

// Suppress the unused field warning — r_id is part of the schema
impl RawGraphRow {
    #[allow(dead_code)]
    fn relation_id(&self) -> &str {
        &self.r_id
    }
}

fn parse_entity(
    id: &str,
    etype: &str,
    name: &str,
    props: &str,
    created: &str,
    updated: &str,
) -> Entity {
    let entity_type: EntityType =
        serde_json::from_str(etype).unwrap_or(EntityType::Custom("unknown".to_string()));
    let properties: HashMap<String, serde_json::Value> =
        serde_json::from_str(props).unwrap_or_default();
    let created_at = chrono::DateTime::parse_from_rfc3339(created)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    let updated_at = chrono::DateTime::parse_from_rfc3339(updated)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    Entity {
        id: id.to_string(),
        entity_type,
        name: name.to_string(),
        properties,
        created_at,
        updated_at,
    }
}

fn parse_relation(
    source: &str,
    rtype: &str,
    target: &str,
    props: &str,
    confidence: f64,
    created: &str,
) -> Relation {
    let relation: RelationType = serde_json::from_str(rtype).unwrap_or(RelationType::RelatedTo);
    let properties: HashMap<String, serde_json::Value> =
        serde_json::from_str(props).unwrap_or_default();
    let created_at = chrono::DateTime::parse_from_rfc3339(created)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    Relation {
        source: source.to_string(),
        relation,
        target: target.to_string(),
        properties,
        confidence: confidence as f32,
        created_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration::run_migrations;

    fn setup() -> KnowledgeStore {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        KnowledgeStore::new(Arc::new(Mutex::new(conn)))
    }

    #[test]
    fn test_add_and_query_entity() {
        let store = setup();
        let id = store
            .add_entity(Entity {
                id: String::new(),
                entity_type: EntityType::Person,
                name: "Alice".to_string(),
                properties: HashMap::new(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            })
            .unwrap();
        assert!(!id.is_empty());
    }

    #[test]
    fn test_add_relation_and_query() {
        let store = setup();
        let alice_id = store
            .add_entity(Entity {
                id: "alice".to_string(),
                entity_type: EntityType::Person,
                name: "Alice".to_string(),
                properties: HashMap::new(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            })
            .unwrap();
        let company_id = store
            .add_entity(Entity {
                id: "acme".to_string(),
                entity_type: EntityType::Organization,
                name: "Acme Corp".to_string(),
                properties: HashMap::new(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            })
            .unwrap();
        store
            .add_relation(Relation {
                source: alice_id.clone(),
                relation: RelationType::WorksAt,
                target: company_id,
                properties: HashMap::new(),
                confidence: 0.95,
                created_at: Utc::now(),
            })
            .unwrap();

        let matches = store
            .query_graph(GraphPattern {
                source: Some(alice_id),
                relation: Some(RelationType::WorksAt),
                target: None,
                max_depth: 1,
            })
            .unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].target.name, "Acme Corp");
    }
}
