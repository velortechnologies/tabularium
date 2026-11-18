//! Shared index-operation ownership and durable derived lexical revision stamps.
use std::path::Path;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

use super::{Database, EntryId, Storage};
use crate::{Error, Result, SearchKind};

pub(super) struct Indexing {
    pub mutex: Mutex<()>,
    permits: Arc<Semaphore>,
    state: SqlitePool,
}

/// Permit ownership is the registration: cancellation/unwinding releases it exactly once.
pub(super) struct Operation {
    _permit: OwnedSemaphorePermit,
}

impl Indexing {
    pub async fn open(path: &Path) -> Result<Self> {
        let state = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path.join("index-state.db"))
                    .create_if_missing(true)
                    .busy_timeout(std::time::Duration::from_secs(5)),
            )
            .await?;
        sqlx::query("CREATE TABLE IF NOT EXISTS lexical_state (id INTEGER PRIMARY KEY, stamp TEXT NOT NULL)")
            .execute(&state).await?;
        Ok(Self {
            mutex: Mutex::new(()),
            permits: Arc::new(Semaphore::new(Semaphore::MAX_PERMITS)),
            state,
        })
    }
    pub fn register(&self) -> Operation {
        Operation {
            _permit: self
                .permits
                .clone()
                .try_acquire_owned()
                .expect("index operation capacity exhausted"),
        }
    }
    pub fn active(&self) -> usize {
        Semaphore::MAX_PERMITS - self.permits.available_permits()
    }
    async fn matches(&self, id: EntryId, stamp: &str) -> Result<bool> {
        let stored: Option<String> =
            sqlx::query_scalar("SELECT stamp FROM lexical_state WHERE id=?")
                .bind(id.raw())
                .fetch_optional(&self.state)
                .await?;
        Ok(stored.as_deref() == Some(stamp))
    }
    async fn mark(&self, id: EntryId, stamp: &str) -> Result<()> {
        sqlx::query("INSERT OR REPLACE INTO lexical_state(id,stamp) VALUES(?,?)")
            .bind(id.raw())
            .bind(stamp)
            .execute(&self.state)
            .await?;
        Ok(())
    }
    pub async fn forget(&self, id: EntryId) -> Result<()> {
        sqlx::query("DELETE FROM lexical_state WHERE id=?")
            .bind(id.raw())
            .execute(&self.state)
            .await?;
        Ok(())
    }
}

/// Current engine state. Disabled and ineligible engines are not pending work.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexState {
    pub path: String,
    pub lexical: &'static str,
    pub semantic: &'static str,
}
impl IndexState {
    pub(super) fn missing(&self, kind: SearchKind) -> bool {
        (kind != SearchKind::Semantic && self.lexical == "pending")
            || (kind != SearchKind::Lexical && self.semantic == "pending")
    }
}

impl<S: Storage> Database<S> {
    async fn lexical_document(
        &self,
        id: EntryId,
    ) -> Result<(String, (String, String, String, String))> {
        let before = self.storage.get_file_meta(id).await?;
        let fields = self.storage.file_search_index_fields(id).await?;
        let after = self.storage.get_file_meta(id).await?;
        if before.revision() != after.revision()
            || before.canonical_path() != after.canonical_path()
        {
            return Err(Error::RevisionMismatch(
                "document changed while preparing index".into(),
            ));
        }
        // Descriptions/path changes are included even on backends without revision bumps.
        let bytes = serde_json::to_vec(&(after.revision(), after.canonical_path(), &fields))
            .map_err(|e| Error::InvalidInput(e.to_string()))?;
        Ok((format!("{:x}", Sha256::digest(bytes)), fields))
    }
    /// Inspect per-engine indexed state against current source revision/content.
    pub async fn document_index_state(&self, id: EntryId) -> Result<IndexState> {
        let (stamp, _) = self.lexical_document(id).await?;
        let meta = self.storage.get_file_meta(id).await?;
        let lexical = if self.indexing.matches(id, &stamp).await? {
            "ready"
        } else {
            "pending"
        };
        let semantic = if let Some(index) = &self.semantic {
            if meta.is_binary() {
                "not_applicable"
            } else if matches!(index.state().await?, "unindexed" | "incompatible") {
                "pending"
            } else if index.matches(&self.semantic_document(id).await?).await? {
                "ready"
            } else {
                "pending"
            }
        } else {
            "disabled"
        };
        Ok(IndexState {
            path: meta.canonical_path().into(),
            lexical,
            semantic,
        })
    }
    /// Caller holds the shared index mutex. Model/index errors abort a job;
    /// source-document errors can be skipped. Revision stamps never bless newer writes.
    pub(super) async fn index_one(
        &self,
        id: EntryId,
        kind: SearchKind,
        missing_only: bool,
    ) -> Result<()> {
        if kind != SearchKind::Semantic {
            let (stamp, (dp, name, desc, content)) = self.lexical_document(id).await?;
            if !missing_only || !self.indexing.matches(id, &stamp).await? {
                self.search
                    .upsert_file(id, &dp, &name, &desc, &content)
                    .await?;
                self.indexing.mark(id, &stamp).await?;
            }
        }
        if kind != SearchKind::Lexical
            && let Some(index) = &self.semantic
        {
            if self.storage.get_file_meta(id).await?.is_binary() {
                index.forget(id.raw()).await?;
            } else {
                let doc = self.semantic_document(id).await?;
                if !missing_only || !index.matches(&doc).await? {
                    index.mark_pending(id.raw()).await?;
                    index.replace_one(&doc).await?;
                }
            }
        }
        Ok(())
    }
    pub(super) async fn inline_index(&self, id: EntryId) {
        let _operation = self.indexing.register();
        if let Some(index) = &self.semantic
            && let Err(error) = index.mark_pending(id.raw()).await
        {
            tracing::error!(file_id=%id, %error, "could not mark semantic revision pending");
        }
        let _guard = self.indexing.mutex.lock().await;
        if let Err(error) = self.index_one(id, SearchKind::Hybrid, false).await {
            tracing::error!(file_id=%id, %error, "document saved; indexing failed; reindex required");
        }
    }
    pub(super) async fn remove_from_indexes(&self, id: EntryId) -> Result<()> {
        let _operation = self.indexing.register();
        let _guard = self.indexing.mutex.lock().await;
        self.search.delete_file(id).await?;
        self.indexing.forget(id).await?;
        if let Some(index) = &self.semantic {
            index.forget(id.raw()).await?;
        }
        Ok(())
    }
}
