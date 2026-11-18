//! Shared index-operation ownership and source-transactional indexing flags.
use std::path::Path;
use std::sync::Arc;

use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

use super::{Database, EntryId, Storage};
use crate::{Result, SearchKind};

pub(super) struct Indexing {
    pub mutex: Mutex<()>,
    permits: Arc<Semaphore>,
}

/// Permit ownership is the registration: cancellation/unwinding releases it exactly once.
pub(super) struct Operation {
    _permit: OwnedSemaphorePermit,
}

impl Indexing {
    pub async fn open(_path: &Path) -> Result<Self> {
        Ok(Self {
            mutex: Mutex::new(()),
            permits: Arc::new(Semaphore::new(Semaphore::MAX_PERMITS)),
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
}

/// Current engine state. Disabled and ineligible engines are not pending work.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexState {
    pub path: String,
    pub lexical: &'static str,
    pub semantic: &'static str,
}
impl<S: Storage> Database<S> {
    /// Indexed flags are committed in the source database, never inferred from bodies.
    pub async fn document_index_state(&self, id: EntryId) -> Result<IndexState> {
        let (_, lexical, semantic) = self.storage.index_generation(id).await?;
        let meta = self.storage.get_file_meta(id).await?;
        Ok(IndexState {
            path: meta.canonical_path().into(),
            lexical: if lexical { "ready" } else { "pending" },
            semantic: if self.semantic.is_none() {
                "disabled"
            } else if meta.is_binary() {
                "not_applicable"
            } else if semantic {
                "ready"
            } else {
                "pending"
            },
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
        let (generation, lexical, semantic) = self.storage.index_generation(id).await?;
        if kind != SearchKind::Semantic && (!missing_only || !lexical) {
            self.storage.prepare_index(id, false).await?;
            let (dp, name, desc, content) = self.storage.file_search_index_fields(id).await?;
            self.search
                .upsert_file(id, &dp, &name, &desc, &content)
                .await?;
            self.storage.mark_indexed(id, generation, false).await?;
        }
        if kind != SearchKind::Lexical
            && let Some(index) = &self.semantic
        {
            if self.storage.get_file_meta(id).await?.is_binary() {
                index.forget(id.raw()).await?;
            } else if !missing_only || !semantic {
                self.storage.prepare_index(id, true).await?;
                let doc = self.semantic_document(id).await?;
                index.mark_pending(id.raw()).await?;
                index.replace_one(&doc).await?;
                self.storage.mark_indexed(id, generation, true).await?;
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
        if let Some(index) = &self.semantic {
            index.forget(id.raw()).await?;
        }
        Ok(())
    }
}
