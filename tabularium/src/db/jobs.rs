//! Server-owned, volatile jobs. Cancellation is cooperative at committed work boundaries.
use super::{Database, EntryId, Storage};
use crate::{Error, Result, SearchKind};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReindexOptions {
    pub path: String,
    #[serde(default)]
    pub non_indexed_only: bool,
    #[serde(default)]
    pub indexer_kind: SearchKind,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReindexJob {
    pub id: String,
    #[serde(flatten)]
    pub options: ReindexOptions,
    pub processed: usize,
    pub skipped: usize,
    pub total: Option<usize>,
}

struct Running {
    status: ReindexJob,
    cancel: watch::Sender<bool>,
    done: watch::Receiver<bool>,
}
#[derive(Default)]
pub(super) struct Jobs {
    running: Arc<Mutex<HashMap<String, Running>>>,
}
struct Cleanup {
    running: Arc<Mutex<HashMap<String, Running>>>,
    id: String,
    done: watch::Sender<bool>,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        self.running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.id);
        self.done.send_replace(true);
    }
}
fn document_error(error: &Error) -> bool {
    matches!(
        error,
        Error::NotFound(_)
            | Error::BinaryEntry(_)
            | Error::RevisionMismatch(_)
            | Error::InvalidInput(_)
    )
}
impl<S: Storage> Database<S> {
    async fn selected_ids(&self, options: &ReindexOptions) -> Result<Vec<EntryId>> {
        crate::resource_path::canonical_path_segments(&options.path)?;
        if options.indexer_kind == SearchKind::Semantic && self.semantic.is_none() {
            return Err(Error::InvalidInput(
                "semantic search is not configured".into(),
            ));
        }
        let path = (options.path != "/").then_some(options.path.as_str());
        let rows = self.storage.files_for_search_reindex(path).await?;
        let mut ids = Vec::with_capacity(rows.len());
        for (id, _, _, _, _) in rows {
            let missing = if options.non_indexed_only {
                match self.document_index_state(id).await {
                    Ok(state) => state.missing(options.indexer_kind),
                    Err(error) if document_error(&error) => {
                        tracing::warn!(file_id=%id, %error, "reindex selection skipping document");
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            } else {
                true
            };
            if missing {
                ids.push(id);
            }
        }
        ids.sort_by_key(|id| id.raw());
        Ok(ids)
    }
    pub async fn reindex_explain(
        &self,
        options: &ReindexOptions,
    ) -> Result<Vec<super::IndexState>> {
        let mut result = Vec::new();
        for id in self.selected_ids(options).await? {
            result.push(self.document_index_state(id).await?);
        }
        Ok(result)
    }
    pub fn reindex_jobs(&self) -> Vec<ReindexJob> {
        let jobs = self
            .jobs
            .running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut result: Vec<_> = jobs.values().map(|j| j.status.clone()).collect();
        result.sort_by(|a, b| a.id.cmp(&b.id));
        result
    }
    pub fn reindex_job_status(&self, id: &str) -> Result<ReindexJob> {
        self.jobs
            .running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .map(|j| j.status.clone())
            .ok_or_else(|| Error::NotFound(id.into()))
    }
    /// Waits for the current work unit to settle before returning/removing the job.
    pub async fn reindex_job_stop(&self, id: &str) -> Result<()> {
        let mut done = {
            let jobs = self
                .jobs
                .running
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let job = jobs.get(id).ok_or_else(|| Error::NotFound(id.into()))?;
            job.cancel.send_replace(true);
            job.done.clone()
        };
        while !*done.borrow_and_update() {
            if done.changed().await.is_err() {
                break;
            }
        }
        Ok(())
    }
    pub(super) async fn reindex_selected(
        &self,
        path: &str,
        kind: SearchKind,
        missing: bool,
    ) -> Result<()> {
        let options = ReindexOptions {
            path: path.into(),
            indexer_kind: kind,
            non_indexed_only: missing,
        };
        let (_tx, mut cancel) = watch::channel(false);
        self.run_reindex(&options, None, &mut cancel).await
    }
    async fn run_reindex(
        &self,
        options: &ReindexOptions,
        job_id: Option<&str>,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<()> {
        let _operation = self.indexing.register();
        let ids = tokio::select! {
            biased;
            _ = cancel.changed() => return Ok(()),
            result = self.selected_ids(options) => result?,
        };
        if let Some(id) = job_id
            && let Some(job) = self
                .jobs
                .running
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get_mut(id)
        {
            job.status.total = Some(ids.len());
        }
        let mut guard = tokio::select! {
            biased;
            _ = cancel.changed() => return Ok(()),
            g = self.indexing.mutex.lock() => g,
        };
        let mut slice = Instant::now();
        for id in ids {
            if *cancel.borrow() {
                break;
            }
            // Do not drop an in-flight commit when stop is requested.
            let skipped = match self
                .index_one(id, options.indexer_kind, options.non_indexed_only)
                .await
            {
                Ok(()) => false,
                Err(error) if document_error(&error) => {
                    tracing::warn!(file_id=%id, %error, "reindex skipping document");
                    true
                }
                Err(error) => return Err(error),
            };
            if let Some(id) = job_id
                && let Some(job) = self
                    .jobs
                    .running
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get_mut(id)
            {
                job.status.processed += 1;
                job.status.skipped += usize::from(skipped);
            }
            if slice.elapsed() >= Duration::from_millis(200) {
                drop(guard);
                if self.indexing.active() > 1 {
                    tokio::select! {
                        biased;
                        _ = cancel.changed() => return Ok(()),
                        () = tokio::time::sleep(Duration::from_millis(1)) => {}
                    }
                }
                guard = tokio::select! {
                    biased;
                    _ = cancel.changed() => return Ok(()),
                    g = self.indexing.mutex.lock() => g,
                };
                slice = Instant::now();
            }
        }
        drop(guard);
        if options.path == "/"
            && options.indexer_kind != SearchKind::Lexical
            && !*cancel.borrow()
            && let Some(index) = &self.semantic
        {
            let pending = ReindexOptions {
                path: "/".into(),
                non_indexed_only: true,
                indexer_kind: SearchKind::Semantic,
            };
            if self.selected_ids(&pending).await?.is_empty() {
                let _guard = self.indexing.mutex.lock().await;
                if !*cancel.borrow() {
                    index.mark_complete().await?;
                }
            }
        }
        Ok(())
    }
}
impl<S: Storage + 'static> Database<S> {
    pub async fn reindex_job_start(self: &Arc<Self>, options: ReindexOptions) -> Result<String> {
        self.start_job(options, false).await.and_then(|r| {
            r.map(|(id, _)| id)
                .ok_or_else(|| Error::Search("manual job unexpectedly skipped".into()))
        })
    }
    async fn start_job(
        self: &Arc<Self>,
        options: ReindexOptions,
        skip_equivalent: bool,
    ) -> Result<Option<(String, watch::Receiver<bool>)>> {
        crate::resource_path::canonical_path_segments(&options.path)?;
        if options.path != "/" {
            self.resolve_directory_path(&options.path).await?;
        }
        if options.indexer_kind == SearchKind::Semantic && self.semantic.is_none() {
            return Err(Error::InvalidInput(
                "semantic search is not configured".into(),
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (cancel_tx, mut cancel) = watch::channel(false);
        let (done_tx, done) = watch::channel(false);
        let mut jobs = self
            .jobs
            .running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if skip_equivalent && jobs.values().any(|j| j.status.options == options) {
            return Ok(None);
        }
        jobs.insert(
            id.clone(),
            Running {
                status: ReindexJob {
                    id: id.clone(),
                    options: options.clone(),
                    processed: 0,
                    skipped: 0,
                    total: None,
                },
                cancel: cancel_tx,
                done: done.clone(),
            },
        );
        let cleanup = Cleanup {
            running: self.jobs.running.clone(),
            id: id.clone(),
            done: done_tx,
        };
        let db = self.clone();
        tokio::spawn(async move {
            let _cleanup = cleanup;
            let result = db
                .run_reindex(&options, Some(&_cleanup.id), &mut cancel)
                .await;
            let progress = db.reindex_job_status(&_cleanup.id).ok();
            match result {
                Ok(()) => {
                    tracing::info!(job_id=%_cleanup.id, stopped=*cancel.borrow(), ?progress, "reindex job finished");
                }
                Err(error) => {
                    tracing::error!(job_id=%_cleanup.id, %error, ?progress, "reindex job stopped on engine failure");
                }
            }
        });
        Ok(Some((id, done)))
    }
    /// The caller owns/aborts this task at shutdown. No persistent work is created.
    pub fn start_auto_reindex(
        self: &Arc<Self>,
        seconds: u64,
    ) -> Result<tokio::task::JoinHandle<()>> {
        if seconds == 0 {
            return Err(Error::InvalidInput("auto_reindex must be positive".into()));
        }
        let weak = Arc::downgrade(self);
        Ok(tokio::spawn(async move {
            let period = Duration::from_secs(seconds);
            let mut interval =
                tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let Some(db) = weak.upgrade() else { break };
                let options = ReindexOptions {
                    path: "/".into(),
                    non_indexed_only: true,
                    indexer_kind: SearchKind::Hybrid,
                };
                let job = db.start_job(options, true).await;
                drop(db);
                match job {
                    Ok(Some((_, mut done))) => {
                        while !*done.borrow_and_update() {
                            if done.changed().await.is_err() {
                                break;
                            }
                        }
                        interval.reset();
                    }
                    Ok(None) => {}
                    Err(error) => tracing::error!(%error, "auto reindex start failed"),
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteDatabase;
    async fn database() -> (tempfile::TempDir, Arc<SqliteDatabase>) {
        let temp = tempfile::tempdir().unwrap();
        let db = SqliteDatabase::init(
            format!("sqlite://{}", temp.path().join("source.db").display()),
            temp.path().join("index"),
            0,
        )
        .await
        .unwrap();
        (temp, Arc::new(db))
    }
    fn options() -> ReindexOptions {
        ReindexOptions {
            path: "/".into(),
            non_indexed_only: true,
            indexer_kind: SearchKind::Hybrid,
        }
    }
    async fn wait(db: &SqliteDatabase) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !db.reindex_jobs().is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn stop_waiting_job_and_atomic_auto_dedup() {
        let (_temp, db) = database().await;
        let guard = db.indexing.mutex.lock().await;
        let id = db.reindex_job_start(options()).await.unwrap();
        assert_eq!(db.reindex_job_status(&id).unwrap().options, options());
        assert!(db.start_job(options(), true).await.unwrap().is_none());
        // A second manual job is allowed; this is not a queue/concurrency limit.
        let second = db.reindex_job_start(options()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(db.indexing.active(), 2);
        db.reindex_job_stop(&id).await.unwrap();
        assert_eq!(db.indexing.active(), 1);
        db.reindex_job_stop(&second).await.unwrap();
        assert_eq!(db.indexing.active(), 0);
        assert!(db.reindex_jobs().is_empty());
        drop(guard);
    }
    #[tokio::test]
    async fn permits_drop_on_abort_unwind_and_normal_completion() {
        let (_temp, db) = database().await;
        let permit = db.indexing.register();
        assert_eq!(db.indexing.active(), 1);
        drop(permit);
        let other = db.clone();
        let task = tokio::spawn(async move {
            let _permit = other.indexing.register();
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        assert_eq!(db.indexing.active(), 1);
        task.abort();
        let _ = task.await;
        assert_eq!(db.indexing.active(), 0);
        let other = db.clone();
        let task = tokio::spawn(async move {
            let _permit = other.indexing.register();
            panic!("test unwind");
        });
        assert!(task.await.is_err());
        assert_eq!(db.indexing.active(), 0);
    }
    #[tokio::test]
    async fn inline_calls_register_before_wait_and_abort_releases() {
        let (_temp, db) = database().await;
        let id = db
            .create_document_at_path("/file", "one", false, None)
            .await
            .unwrap();
        let guard = db.indexing.mutex.lock().await;
        let other = db.clone();
        let task = tokio::spawn(async move { other.update_document(id, "two").await });
        tokio::time::timeout(Duration::from_secs(5), async {
            while db.indexing.active() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(db.indexing.active(), 1);
        task.abort();
        let _ = task.await;
        assert_eq!(db.indexing.active(), 0);
        drop(guard);
        assert_eq!(
            db.document_index_state(id).await.unwrap().lexical,
            "pending"
        );
        db.reindex_job_start(options()).await.unwrap();
        wait(&db).await;
        assert_eq!(db.document_index_state(id).await.unwrap().lexical, "ready");
    }
    #[tokio::test]
    async fn automatic_repair_and_shutdown() {
        let (_temp, db) = database().await;
        assert!(db.start_auto_reindex(0).is_err());
        // Simulate a source write followed by process loss before index commit.
        db.storage
            .create_file("/missing", "repair me")
            .await
            .unwrap();
        let scheduler = db.start_auto_reindex(1).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if db.reindex_explain(&options()).await.unwrap().is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        wait(&db).await;
        scheduler.abort();
        let _ = scheduler.await;
        assert_eq!(db.indexing.active(), 0);
    }
}
