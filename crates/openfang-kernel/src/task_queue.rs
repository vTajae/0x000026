//! Task priority queue — inter-agent task delegation with prioritization.
//!
//! Enables agents to submit work items for other agents (or themselves) with
//! priority levels, deadlines, and result tracking. The queue is ordered by
//! priority, then by submission time (FIFO within same priority).
//!
//! This bridges the gap between the event bus (fire-and-forget notifications)
//! and workflows (pre-defined step sequences) by enabling dynamic, on-demand
//! task delegation between agents.

use openfang_types::agent::AgentId;
use serde::{Deserialize, Serialize};
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tracing::debug;

// ---------------------------------------------------------------------------
// Task types
// ---------------------------------------------------------------------------

/// Unique task ID.
pub type TaskId = u64;

/// Task priority levels (higher number = higher priority).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TaskPriority {
    /// Background work — process when idle.
    Low = 0,
    /// Normal operational tasks.
    #[default]
    Normal = 1,
    /// Time-sensitive or user-facing tasks.
    High = 2,
    /// System-critical tasks (safety, approval responses).
    Critical = 3,
}

/// Current state of a queued task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    /// Waiting in the queue.
    Queued,
    /// Currently being processed by the target agent.
    Running,
    /// Completed successfully.
    Completed,
    /// Failed with an error.
    Failed,
    /// Cancelled before completion.
    Cancelled,
    /// Deadline expired before processing.
    Expired,
}

/// A task submitted to the priority queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedTask {
    /// Unique task identifier.
    pub id: TaskId,
    /// Agent that submitted the task.
    pub source_agent: AgentId,
    /// Agent that should process the task (None = any available).
    pub target_agent: Option<AgentId>,
    /// Priority level.
    pub priority: TaskPriority,
    /// The task description / prompt.
    pub payload: String,
    /// Additional structured data.
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    /// Current state.
    pub state: TaskState,
    /// Result (populated on completion).
    #[serde(default)]
    pub result: Option<String>,
    /// Error message (populated on failure).
    #[serde(default)]
    pub error: Option<String>,
    /// When the task was submitted (epoch millis for serde).
    pub submitted_at_ms: u64,
    /// Optional deadline (epoch millis).
    #[serde(default)]
    pub deadline_ms: Option<u64>,
    /// Number of retry attempts.
    #[serde(default)]
    pub retry_count: u32,
    /// Maximum retries allowed.
    #[serde(default)]
    pub max_retries: u32,
}

/// Internal heap entry for priority ordering.
#[derive(Debug)]
struct HeapEntry {
    task_id: TaskId,
    priority: TaskPriority,
    /// Sequence number for FIFO within same priority (lower = earlier).
    seq: u64,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.seq == other.seq
    }
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Higher priority first, then lower seq (earlier) first
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

/// Internal action from take() to avoid holding DashMap RefMut across iterations.
enum TakeAction {
    Found(QueuedTask),
    WrongAgent,
    Skip,
}

// ---------------------------------------------------------------------------
// Task Queue
// ---------------------------------------------------------------------------

/// Priority queue for inter-agent task delegation.
pub struct TaskQueue {
    /// Priority heap for pending tasks.
    heap: Mutex<BinaryHeap<HeapEntry>>,
    /// All tasks indexed by ID (including completed).
    tasks: dashmap::DashMap<TaskId, QueuedTask>,
    /// Next task ID.
    next_id: AtomicU64,
    /// Sequence counter for FIFO ordering.
    next_seq: AtomicU64,
    /// Maximum queue depth (prevents unbounded growth).
    max_queue_size: usize,
    /// Maximum completed tasks to retain.
    max_history: usize,
}

impl TaskQueue {
    /// Create a new task queue.
    pub fn new() -> Self {
        Self {
            heap: Mutex::new(BinaryHeap::with_capacity(256)),
            tasks: dashmap::DashMap::new(),
            next_id: AtomicU64::new(1),
            next_seq: AtomicU64::new(0),
            max_queue_size: 10_000,
            max_history: 1_000,
        }
    }

    /// Submit a new task to the queue.
    pub fn submit(
        &self,
        source: AgentId,
        target: Option<AgentId>,
        priority: TaskPriority,
        payload: String,
        metadata: Option<serde_json::Value>,
        deadline: Option<Duration>,
    ) -> Result<TaskId, TaskQueueError> {
        // Check queue capacity
        let heap_len = self.heap.lock().unwrap().len();
        if heap_len >= self.max_queue_size {
            return Err(TaskQueueError::QueueFull);
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let deadline_ms = deadline.map(|d| now + d.as_millis() as u64);

        let task = QueuedTask {
            id,
            source_agent: source,
            target_agent: target,
            priority,
            payload,
            metadata,
            state: TaskState::Queued,
            result: None,
            error: None,
            submitted_at_ms: now,
            deadline_ms,
            retry_count: 0,
            max_retries: 2,
        };

        self.tasks.insert(id, task);
        self.heap.lock().unwrap().push(HeapEntry {
            task_id: id,
            priority,
            seq,
        });

        debug!(task_id = id, priority = ?priority, "Task submitted to queue");
        Ok(id)
    }

    /// Take the highest-priority task for a specific agent (or any unassigned).
    pub fn take(&self, agent_id: &AgentId) -> Option<QueuedTask> {
        let mut heap = self.heap.lock().unwrap();
        let mut skipped = Vec::new();

        while let Some(entry) = heap.pop() {
            // Scope the DashMap RefMut to avoid holding it across iterations
            let action = {
                if let Some(mut task) = self.tasks.get_mut(&entry.task_id) {
                    // Skip expired tasks
                    if let Some(deadline) = task.deadline_ms {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        if now > deadline {
                            task.state = TaskState::Expired;
                            TakeAction::Skip // Drop RefMut, continue
                        } else if task.state != TaskState::Queued {
                            TakeAction::Skip
                        } else {
                            let matches = task
                                .target_agent
                                .as_ref()
                                .is_none_or(|t| t == agent_id);
                            if matches {
                                task.state = TaskState::Running;
                                TakeAction::Found(task.clone())
                            } else {
                                TakeAction::WrongAgent
                            }
                        }
                    } else if task.state != TaskState::Queued {
                        TakeAction::Skip
                    } else {
                        let matches = task
                            .target_agent
                            .as_ref()
                            .is_none_or(|t| t == agent_id);
                        if matches {
                            task.state = TaskState::Running;
                            TakeAction::Found(task.clone())
                        } else {
                            TakeAction::WrongAgent
                        }
                    }
                } else {
                    TakeAction::Skip
                }
            }; // RefMut dropped here

            match action {
                TakeAction::Found(task) => {
                    for s in skipped {
                        heap.push(s);
                    }
                    return Some(task);
                }
                TakeAction::WrongAgent => {
                    skipped.push(entry);
                }
                TakeAction::Skip => {}
            }
        }

        // Re-add all skipped
        for s in skipped {
            heap.push(s);
        }
        None
    }

    /// Complete a task with a result.
    pub fn complete(&self, task_id: TaskId, result: String) -> Result<(), TaskQueueError> {
        {
            let mut task = self
                .tasks
                .get_mut(&task_id)
                .ok_or(TaskQueueError::NotFound(task_id))?;

            if task.state != TaskState::Running {
                return Err(TaskQueueError::InvalidState {
                    task_id,
                    expected: TaskState::Running,
                    actual: task.state,
                });
            }

            task.state = TaskState::Completed;
            task.result = Some(result);
        } // RefMut dropped before gc_history

        debug!(task_id, "Task completed");
        self.gc_history();
        Ok(())
    }

    /// Fail a task with an error. Retries if attempts remain.
    pub fn fail(&self, task_id: TaskId, error: String) -> Result<bool, TaskQueueError> {
        let mut task = self
            .tasks
            .get_mut(&task_id)
            .ok_or(TaskQueueError::NotFound(task_id))?;

        if task.state != TaskState::Running {
            return Err(TaskQueueError::InvalidState {
                task_id,
                expected: TaskState::Running,
                actual: task.state,
            });
        }

        task.retry_count += 1;

        if task.retry_count <= task.max_retries {
            // Re-queue for retry
            task.state = TaskState::Queued;
            task.error = Some(format!("Attempt {}: {error}", task.retry_count));
            let priority = task.priority;
            drop(task);

            let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
            self.heap.lock().unwrap().push(HeapEntry {
                task_id,
                priority,
                seq,
            });

            debug!(task_id, retry = true, "Task failed, re-queued");
            Ok(true) // retried
        } else {
            task.state = TaskState::Failed;
            task.error = Some(error);
            debug!(task_id, retry = false, "Task failed permanently");
            Ok(false) // not retried
        }
    }

    /// Cancel a task.
    pub fn cancel(&self, task_id: TaskId) -> Result<(), TaskQueueError> {
        let mut task = self
            .tasks
            .get_mut(&task_id)
            .ok_or(TaskQueueError::NotFound(task_id))?;

        match task.state {
            TaskState::Queued | TaskState::Running => {
                task.state = TaskState::Cancelled;
                Ok(())
            }
            _ => Err(TaskQueueError::InvalidState {
                task_id,
                expected: TaskState::Queued,
                actual: task.state,
            }),
        }
    }

    /// Get a task by ID.
    pub fn get(&self, task_id: TaskId) -> Option<QueuedTask> {
        self.tasks.get(&task_id).map(|t| t.clone())
    }

    /// Get all tasks for a source agent.
    pub fn tasks_by_source(&self, agent_id: &AgentId) -> Vec<QueuedTask> {
        self.tasks
            .iter()
            .filter(|t| t.source_agent == *agent_id)
            .map(|t| t.clone())
            .collect()
    }

    /// Get all tasks targeting a specific agent.
    pub fn tasks_for_agent(&self, agent_id: &AgentId) -> Vec<QueuedTask> {
        self.tasks
            .iter()
            .filter(|t| t.target_agent.as_ref() == Some(agent_id))
            .map(|t| t.clone())
            .collect()
    }

    /// Get pending task count.
    pub fn pending_count(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| t.state == TaskState::Queued)
            .count()
    }

    /// Get queue stats.
    pub fn stats(&self) -> QueueStats {
        let mut queued = 0;
        let mut running = 0;
        let mut completed = 0;
        let mut failed = 0;

        for entry in self.tasks.iter() {
            match entry.state {
                TaskState::Queued => queued += 1,
                TaskState::Running => running += 1,
                TaskState::Completed => completed += 1,
                TaskState::Failed | TaskState::Cancelled | TaskState::Expired => failed += 1,
            }
        }

        QueueStats {
            queued,
            running,
            completed,
            failed,
            total: self.tasks.len(),
        }
    }

    /// Expire overdue tasks.
    pub fn expire_overdue(&self) -> usize {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let mut expired = 0;
        for mut entry in self.tasks.iter_mut() {
            if entry.state == TaskState::Queued {
                if let Some(deadline) = entry.deadline_ms {
                    if now > deadline {
                        entry.state = TaskState::Expired;
                        expired += 1;
                    }
                }
            }
        }
        expired
    }

    /// Garbage collect completed/failed tasks beyond max_history.
    fn gc_history(&self) {
        let terminal: Vec<TaskId> = self
            .tasks
            .iter()
            .filter(|t| matches!(t.state, TaskState::Completed | TaskState::Failed | TaskState::Cancelled | TaskState::Expired))
            .map(|t| t.id)
            .collect();

        if terminal.len() > self.max_history {
            let excess = terminal.len() - self.max_history;
            // Remove oldest (lowest IDs)
            let mut to_remove: Vec<TaskId> = terminal;
            to_remove.sort_unstable();
            for id in to_remove.into_iter().take(excess) {
                self.tasks.remove(&id);
            }
        }
    }
}

impl Default for TaskQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// Queue statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueStats {
    pub queued: usize,
    pub running: usize,
    pub completed: usize,
    pub failed: usize,
    pub total: usize,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum TaskQueueError {
    QueueFull,
    NotFound(TaskId),
    InvalidState {
        task_id: TaskId,
        expected: TaskState,
        actual: TaskState,
    },
}

impl std::fmt::Display for TaskQueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QueueFull => write!(f, "Task queue is full"),
            Self::NotFound(id) => write!(f, "Task {id} not found"),
            Self::InvalidState {
                task_id,
                expected,
                actual,
            } => write!(
                f,
                "Task {task_id}: expected state {expected:?}, got {actual:?}"
            ),
        }
    }
}

impl std::error::Error for TaskQueueError {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(seed: &str) -> AgentId {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        seed.hash(&mut hasher);
        let h = hasher.finish();
        let bytes = [
            (h >> 56) as u8,
            (h >> 48) as u8,
            (h >> 40) as u8,
            (h >> 32) as u8,
            (h >> 24) as u8,
            (h >> 16) as u8,
            (h >> 8) as u8,
            h as u8,
            0, 0, 0, 0, 0, 0, 0, 0,
        ];
        AgentId(uuid::Uuid::from_bytes(bytes))
    }

    #[test]
    fn test_submit_and_take() {
        let q = TaskQueue::new();
        let src = agent("agent-a");
        let tgt = agent("agent-b");

        let id = q
            .submit(
                src,
                Some(tgt),
                TaskPriority::Normal,
                "Do something".into(),
                None,
                None,
            )
            .unwrap();

        // Wrong agent can't take it
        assert!(q.take(&agent("agent-c")).is_none());

        // Correct agent takes it
        let task = q.take(&tgt).unwrap();
        assert_eq!(task.id, id);
        assert_eq!(task.state, TaskState::Running);
        assert_eq!(task.payload, "Do something");
    }

    #[test]
    fn test_unassigned_task() {
        let q = TaskQueue::new();
        let id = q
            .submit(
                agent("src"),
                None, // any agent
                TaskPriority::Normal,
                "open task".into(),
                None,
                None,
            )
            .unwrap();

        let task = q.take(&agent("anyone")).unwrap();
        assert_eq!(task.id, id);
    }

    #[test]
    fn test_priority_ordering() {
        let q = TaskQueue::new();
        let src = agent("src");

        let low = q
            .submit(src, None, TaskPriority::Low, "low".into(), None, None)
            .unwrap();
        let high = q
            .submit(src, None, TaskPriority::High, "high".into(), None, None)
            .unwrap();
        let normal = q
            .submit(src, None, TaskPriority::Normal, "normal".into(), None, None)
            .unwrap();

        let first = q.take(&agent("w")).unwrap();
        assert_eq!(first.id, high);

        let second = q.take(&agent("w")).unwrap();
        assert_eq!(second.id, normal);

        let third = q.take(&agent("w")).unwrap();
        assert_eq!(third.id, low);
    }

    #[test]
    fn test_fifo_within_priority() {
        let q = TaskQueue::new();
        let src = agent("src");

        let first = q
            .submit(src, None, TaskPriority::Normal, "first".into(), None, None)
            .unwrap();
        let second = q
            .submit(src, None, TaskPriority::Normal, "second".into(), None, None)
            .unwrap();

        let t1 = q.take(&agent("w")).unwrap();
        assert_eq!(t1.id, first);

        let t2 = q.take(&agent("w")).unwrap();
        assert_eq!(t2.id, second);
    }

    #[test]
    fn test_complete() {
        let q = TaskQueue::new();
        let id = q
            .submit(agent("s"), None, TaskPriority::Normal, "task".into(), None, None)
            .unwrap();

        let _ = q.take(&agent("w")).unwrap();
        q.complete(id, "done!".into()).unwrap();

        let task = q.get(id).unwrap();
        assert_eq!(task.state, TaskState::Completed);
        assert_eq!(task.result.as_deref(), Some("done!"));
    }

    #[test]
    fn test_fail_with_retry() {
        let q = TaskQueue::new();
        let id = q
            .submit(agent("s"), None, TaskPriority::Normal, "task".into(), None, None)
            .unwrap();

        let _ = q.take(&agent("w")).unwrap();
        let retried = q.fail(id, "oops".into()).unwrap();
        assert!(retried);

        // Should be back in queue
        let task = q.get(id).unwrap();
        assert_eq!(task.state, TaskState::Queued);
        assert_eq!(task.retry_count, 1);

        // Take again
        let _ = q.take(&agent("w")).unwrap();
        let retried = q.fail(id, "oops again".into()).unwrap();
        assert!(retried);

        // Take again (max retries = 2)
        let _ = q.take(&agent("w")).unwrap();
        let retried = q.fail(id, "final failure".into()).unwrap();
        assert!(!retried);

        let task = q.get(id).unwrap();
        assert_eq!(task.state, TaskState::Failed);
    }

    #[test]
    fn test_cancel() {
        let q = TaskQueue::new();
        let id = q
            .submit(agent("s"), None, TaskPriority::Normal, "task".into(), None, None)
            .unwrap();

        q.cancel(id).unwrap();
        let task = q.get(id).unwrap();
        assert_eq!(task.state, TaskState::Cancelled);

        // Can't take cancelled task
        assert!(q.take(&agent("w")).is_none());
    }

    #[test]
    fn test_stats() {
        let q = TaskQueue::new();
        let src = agent("s");

        q.submit(src, None, TaskPriority::Normal, "a".into(), None, None).unwrap();
        q.submit(src, None, TaskPriority::Normal, "b".into(), None, None).unwrap();
        q.submit(src, None, TaskPriority::Normal, "c".into(), None, None).unwrap();

        let stats = q.stats();
        assert_eq!(stats.queued, 3);
        assert_eq!(stats.total, 3);

        let taken = q.take(&agent("w")).unwrap();
        let stats = q.stats();
        assert_eq!(stats.queued, 2);
        assert_eq!(stats.running, 1);

        q.complete(taken.id, "ok".into()).unwrap();
        let stats = q.stats();
        assert_eq!(stats.completed, 1);
    }

    #[test]
    fn test_tasks_by_source() {
        let q = TaskQueue::new();
        let a = agent("agent-a");
        let b = agent("agent-b");

        q.submit(a, None, TaskPriority::Normal, "a1".into(), None, None).unwrap();
        q.submit(a, None, TaskPriority::Normal, "a2".into(), None, None).unwrap();
        q.submit(b, None, TaskPriority::Normal, "b1".into(), None, None).unwrap();

        assert_eq!(q.tasks_by_source(&a).len(), 2);
    }

    #[test]
    fn test_expire_overdue() {
        let q = TaskQueue::new();
        let id = q
            .submit(
                agent("s"),
                None,
                TaskPriority::Normal,
                "task".into(),
                None,
                Some(Duration::from_millis(0)), // already expired
            )
            .unwrap();

        // Give it a moment to pass deadline
        std::thread::sleep(std::time::Duration::from_millis(5));
        let expired = q.expire_overdue();
        assert_eq!(expired, 1);

        let task = q.get(id).unwrap();
        assert_eq!(task.state, TaskState::Expired);
    }

    #[test]
    fn test_queue_full() {
        let mut q = TaskQueue::new();
        q.max_queue_size = 2;

        q.submit(agent("s"), None, TaskPriority::Normal, "a".into(), None, None).unwrap();
        q.submit(agent("s"), None, TaskPriority::Normal, "b".into(), None, None).unwrap();

        let err = q.submit(agent("s"), None, TaskPriority::Normal, "c".into(), None, None);
        assert!(matches!(err, Err(TaskQueueError::QueueFull)));
    }

    #[test]
    fn test_pending_count() {
        let q = TaskQueue::new();
        q.submit(agent("s"), None, TaskPriority::Normal, "a".into(), None, None).unwrap();
        q.submit(agent("s"), None, TaskPriority::Normal, "b".into(), None, None).unwrap();
        assert_eq!(q.pending_count(), 2);

        let _ = q.take(&agent("w"));
        assert_eq!(q.pending_count(), 1);
    }

    #[test]
    fn test_metadata() {
        let q = TaskQueue::new();
        let meta = serde_json::json!({"tool": "web_fetch", "url": "https://example.com"});
        let id = q
            .submit(
                agent("s"),
                None,
                TaskPriority::High,
                "fetch this".into(),
                Some(meta.clone()),
                None,
            )
            .unwrap();

        let task = q.get(id).unwrap();
        assert_eq!(task.metadata, Some(meta));
    }

    #[test]
    fn test_error_display() {
        let err = TaskQueueError::QueueFull;
        assert!(err.to_string().contains("full"));

        let err = TaskQueueError::NotFound(42);
        assert!(err.to_string().contains("42"));

        let err = TaskQueueError::InvalidState {
            task_id: 7,
            expected: TaskState::Running,
            actual: TaskState::Completed,
        };
        assert!(err.to_string().contains("7"));
    }

    #[test]
    fn test_priority_serde() {
        let p = TaskPriority::Critical;
        let json = serde_json::to_string(&p).unwrap();
        let back: TaskPriority = serde_json::from_str(&json).unwrap();
        assert_eq!(back, TaskPriority::Critical);
    }

    #[test]
    fn test_task_state_serde() {
        let s = TaskState::Completed;
        let json = serde_json::to_string(&s).unwrap();
        let back: TaskState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, TaskState::Completed);
    }
}
