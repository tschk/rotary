use std::sync::Arc;

use parking_lot::Mutex;
pub use zkr::{
    Error as SelfImproveError, MemoryDb, PersonId, SelfImprove as ZkrSelfImprove, TenantId,
};

/// Async wrapper around `zkr::SelfImprove` for use inside the `rx4` agent loop.
///
/// Reflections are written to `zkr` evidence memory in a blocking thread, and
/// relevant lessons are retrieved to augment the system prompt before each run.
#[derive(Clone)]
pub struct SelfImprove {
    inner: Arc<Mutex<ZkrSelfImprove>>,
}

impl SelfImprove {
    pub fn new(db: MemoryDb, tenant_id: TenantId, person_id: PersonId) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ZkrSelfImprove::new(db, tenant_id, person_id))),
        }
    }

    /// Record a reflection from a completed action.
    pub async fn record(
        &self,
        context: &str,
        action: &str,
        outcome: &str,
        lesson: &str,
    ) -> Result<(), SelfImproveError> {
        let inner = Arc::clone(&self.inner);
        let context = context.to_string();
        let action = action.to_string();
        let outcome = outcome.to_string();
        let lesson = lesson.to_string();

        tokio::task::spawn_blocking(move || {
            let mut guard = inner.lock();
            guard
                .record(&context, &action, &outcome, &lesson)
                .map(|_| ())
        })
        .await
        .map_err(|e| SelfImproveError::Invalid(e.to_string()))?
    }

    /// Retrieve relevant lessons and augment the base prompt.
    pub async fn augment(&self, query: &str, base: &str) -> Result<String, SelfImproveError> {
        let inner = Arc::clone(&self.inner);
        let query = query.to_string();
        let base = base.to_string();

        tokio::task::spawn_blocking(move || inner.lock().augment(&query, &base))
            .await
            .map_err(|e| SelfImproveError::Invalid(e.to_string()))?
    }

    /// Retrieve lesson excerpts relevant to `query`.
    pub async fn lessons(&self, query: &str, limit: u32) -> Result<Vec<String>, SelfImproveError> {
        let inner = Arc::clone(&self.inner);
        let query = query.to_string();

        tokio::task::spawn_blocking(move || inner.lock().lessons(&query, limit))
            .await
            .map_err(|e| SelfImproveError::Invalid(e.to_string()))?
    }
}
