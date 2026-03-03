//! Model performance scoring and routing.
//!
//! Tracks per-model quality metrics over time and provides routing
//! recommendations based on historical performance. Scores are updated
//! after each LLM interaction and decayed with a configurable half-life.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};

/// Half-life for score decay (7 days).
const SCORE_HALF_LIFE: Duration = Duration::from_secs(7 * 24 * 3600);

/// Maximum observations to keep per model before pruning oldest.
const MAX_OBSERVATIONS: usize = 1000;

/// A single observation of model performance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelObservation {
    /// When this observation was recorded.
    pub timestamp: SystemTime,
    /// Whether the request succeeded.
    pub success: bool,
    /// Response latency in milliseconds (None if failed before response).
    pub latency_ms: Option<u64>,
    /// Token count (input + output) for cost tracking.
    pub total_tokens: u64,
    /// Task category hint (e.g., "coding", "analysis", "quick_answer").
    pub task_category: Option<String>,
    /// Error category if failed.
    pub error_category: Option<String>,
}

/// Aggregated model score with breakdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelScore {
    /// Overall quality score (0-100).
    pub score: f64,
    /// Success rate (0.0-1.0) over recent observations.
    pub success_rate: f64,
    /// Median latency in milliseconds.
    pub latency_p50_ms: u64,
    /// Total observations recorded.
    pub total_observations: usize,
    /// Recent observations in the decay window.
    pub recent_observations: usize,
    /// Per-category success rates.
    pub category_scores: std::collections::HashMap<String, f64>,
    /// Last updated timestamp.
    pub last_updated: SystemTime,
}

/// The model performance ledger.
pub struct ModelLedger {
    /// Per-model observation history.
    observations: DashMap<String, Vec<ModelObservation>>,
    /// Cached computed scores.
    scores: DashMap<String, ModelScore>,
}

impl ModelLedger {
    /// Create a new empty ledger.
    pub fn new() -> Self {
        Self {
            observations: DashMap::new(),
            scores: DashMap::new(),
        }
    }

    /// Record a new observation for a model.
    pub fn record(&self, model_id: &str, observation: ModelObservation) {
        let mut entry = self.observations.entry(model_id.to_string()).or_default();
        entry.push(observation);

        // Prune oldest if over limit
        if entry.len() > MAX_OBSERVATIONS {
            let drain_count = entry.len() - MAX_OBSERVATIONS;
            entry.drain(..drain_count);
        }

        // Recompute score
        let score = Self::compute_score(&entry);
        drop(entry);
        self.scores.insert(model_id.to_string(), score);
    }

    /// Get the current score for a model.
    pub fn get_score(&self, model_id: &str) -> Option<ModelScore> {
        self.scores.get(model_id).map(|s| s.clone())
    }

    /// Get all model scores as a snapshot.
    pub fn snapshot(&self) -> Vec<(String, ModelScore)> {
        self.scores
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect()
    }

    /// Export all observations as a JSON-serializable snapshot for persistence.
    pub fn export_observations(&self) -> serde_json::Value {
        let map: std::collections::HashMap<String, Vec<ModelObservation>> = self
            .observations
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();
        serde_json::to_value(map).unwrap_or_default()
    }

    /// Import observations from a persisted JSON snapshot, recomputing scores.
    pub fn import_observations(&self, data: &serde_json::Value) {
        if let Ok(map) = serde_json::from_value::<std::collections::HashMap<String, Vec<ModelObservation>>>(data.clone()) {
            for (model_id, obs) in map {
                let score = Self::compute_score(&obs);
                self.observations.insert(model_id.clone(), obs);
                self.scores.insert(model_id, score);
            }
        }
    }

    /// Rank models for a given task category, returning model IDs in preference order.
    pub fn rank_for_task(&self, category: &str) -> Vec<(String, f64)> {
        let mut ranked: Vec<(String, f64)> = self
            .scores
            .iter()
            .map(|entry| {
                let category_score = entry
                    .value()
                    .category_scores
                    .get(category)
                    .copied()
                    .unwrap_or(entry.value().success_rate);
                (entry.key().clone(), category_score * 100.0)
            })
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked
    }

    /// Compute score from observations with time decay.
    fn compute_score(observations: &[ModelObservation]) -> ModelScore {
        let now = SystemTime::now();
        let mut weighted_successes = 0.0f64;
        let mut total_weight = 0.0f64;
        let mut latencies: Vec<u64> = Vec::new();
        let mut category_successes: std::collections::HashMap<String, (f64, f64)> =
            std::collections::HashMap::new();
        let mut recent_count = 0usize;

        for obs in observations {
            let age = now
                .duration_since(obs.timestamp)
                .unwrap_or(Duration::ZERO);
            // Exponential decay weight
            let decay = (-age.as_secs_f64() * std::f64::consts::LN_2
                / SCORE_HALF_LIFE.as_secs_f64())
            .exp();
            let weight = decay.max(0.01); // floor to avoid zero-weight

            if age < Duration::from_secs(7 * 24 * 3600) {
                recent_count += 1;
            }

            total_weight += weight;
            if obs.success {
                weighted_successes += weight;
                if let Some(lat) = obs.latency_ms {
                    latencies.push(lat);
                }
            }

            if let Some(ref cat) = obs.task_category {
                let entry = category_successes
                    .entry(cat.clone())
                    .or_insert((0.0, 0.0));
                entry.1 += weight;
                if obs.success {
                    entry.0 += weight;
                }
            }
        }

        let success_rate = if total_weight > 0.0 {
            weighted_successes / total_weight
        } else {
            0.0
        };

        latencies.sort();
        let latency_p50 = if latencies.is_empty() {
            0
        } else {
            latencies[latencies.len() / 2]
        };

        // Normalize latency score: 0ms = 1.0, 10000ms = 0.0
        let latency_score = (1.0 - (latency_p50 as f64 / 10_000.0).min(1.0)).max(0.0);

        // Overall score: success_rate(40%) + latency(20%) + baseline(40%)
        // Baseline rewards models that have been observed (data > no data)
        let data_confidence = (observations.len() as f64 / 50.0).min(1.0);
        let score =
            (success_rate * 40.0) + (latency_score * 20.0) + (data_confidence * 20.0) + 20.0;

        let category_scores = category_successes
            .into_iter()
            .map(|(cat, (succ, total))| {
                let rate = if total > 0.0 { succ / total } else { 0.0 };
                (cat, rate)
            })
            .collect();

        ModelScore {
            score: score.min(100.0),
            success_rate,
            latency_p50_ms: latency_p50,
            total_observations: observations.len(),
            recent_observations: recent_count,
            category_scores,
            last_updated: now,
        }
    }
}

impl Default for ModelLedger {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_ledger() {
        let ledger = ModelLedger::new();
        assert!(ledger.get_score("test").is_none());
        assert!(ledger.snapshot().is_empty());
    }

    #[test]
    fn test_record_and_score() {
        let ledger = ModelLedger::new();
        for _ in 0..10 {
            ledger.record(
                "qwen3:14b",
                ModelObservation {
                    timestamp: SystemTime::now(),
                    success: true,
                    latency_ms: Some(500),
                    total_tokens: 1000,
                    task_category: Some("coding".to_string()),
                    error_category: None,
                },
            );
        }

        let score = ledger.get_score("qwen3:14b").unwrap();
        assert!(score.score > 50.0);
        assert!((score.success_rate - 1.0).abs() < 0.01);
        assert_eq!(score.latency_p50_ms, 500);
        assert_eq!(score.total_observations, 10);
    }

    #[test]
    fn test_rank_for_task() {
        let ledger = ModelLedger::new();

        // Model A: good at coding
        for _ in 0..5 {
            ledger.record(
                "model-a",
                ModelObservation {
                    timestamp: SystemTime::now(),
                    success: true,
                    latency_ms: Some(200),
                    total_tokens: 500,
                    task_category: Some("coding".to_string()),
                    error_category: None,
                },
            );
        }

        // Model B: bad at coding
        for _ in 0..5 {
            ledger.record(
                "model-b",
                ModelObservation {
                    timestamp: SystemTime::now(),
                    success: false,
                    latency_ms: None,
                    total_tokens: 0,
                    task_category: Some("coding".to_string()),
                    error_category: Some("format".to_string()),
                },
            );
        }

        let ranked = ledger.rank_for_task("coding");
        assert_eq!(ranked[0].0, "model-a");
        assert!(ranked[0].1 > ranked[1].1);
    }

    #[test]
    fn test_max_observations_prune() {
        let ledger = ModelLedger::new();
        for _ in 0..1500 {
            ledger.record(
                "test",
                ModelObservation {
                    timestamp: SystemTime::now(),
                    success: true,
                    latency_ms: Some(100),
                    total_tokens: 100,
                    task_category: None,
                    error_category: None,
                },
            );
        }
        let obs = ledger.observations.get("test").unwrap();
        assert!(obs.len() <= MAX_OBSERVATIONS);
    }
}
