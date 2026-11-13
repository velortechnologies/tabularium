//! Snapshot operations share storage transactions and ordinary index/cache coordination.
use super::{Database, DocumentMeta, EntryId, Storage};
use crate::{Error, Result};

impl<S: Storage> Database<S> {
    /// Explicitly remove a live document and all its snapshots.
    pub async fn delete_document_recursive(&self, id: EntryId) -> Result<()> {
        let snapshots = self.storage.delete_file_recursive(id).await?;
        for snapshot in snapshots {
            self.forget_snapshot_index(snapshot).await?;
        }
        self.forget_snapshot_index(id).await?;
        self.doc_wait.remove(&id);
        self.doc_append_locks.remove(&id);
        Ok(())
    }
    /// Capture documents under a live path. Directory capture requires recursion.
    pub async fn create_snapshot(
        &self,
        path: &str,
        name: &str,
        recursive: bool,
    ) -> Result<Vec<DocumentMeta>> {
        let ids = self.storage.create_snapshots(path, name, recursive).await?;
        let mut result = Vec::new();
        for id in ids {
            self.reindex_file_in_search(id).await?;
            result.push(self.storage.get_file_meta(id).await?);
        }
        Ok(result)
    }

    /// List snapshots in creation order; directories are selectors, never snapshots.
    pub async fn list_snapshots(
        &self,
        path: &str,
        name: Option<&str>,
        recursive: bool,
    ) -> Result<Vec<DocumentMeta>> {
        let ids = self.storage.snapshot_ids(path, name, recursive).await?;
        let mut rows = Vec::new();
        for id in ids {
            rows.push(self.storage.get_file_meta(id).await?);
        }
        Ok(rows)
    }

    /// Remove selected snapshots and their search representations.
    pub async fn delete_snapshot(&self, path: &str, name: &str, recursive: bool) -> Result<()> {
        let ids = self
            .storage
            .change_snapshots(path, name, recursive, "delete", None)
            .await?;
        for id in ids {
            self.forget_snapshot_index(id).await?;
        }
        Ok(())
    }

    pub(super) async fn forget_snapshot_index(&self, id: EntryId) -> Result<()> {
        self.search.delete_file(id).await?;
        if let Some(index) = &self.semantic {
            let _guard = index.operation.lock().await;
            index.forget(id.raw()).await?;
        }
        self.cache.invalidate(&id).await;
        Ok(())
    }

    /// Restore captured state without changing live identity or consuming any snapshot.
    pub async fn rollback_snapshot(&self, path: &str, name: &str, recursive: bool) -> Result<()> {
        let ids = self
            .storage
            .change_snapshots(path, name, recursive, "rollback", None)
            .await?;
        for id in ids {
            self.cache.invalidate(&id).await;
            self.bump_doc_wait(id);
            self.reindex_file_in_search(id).await?;
        }
        Ok(())
    }

    /// Rename snapshots within their existing source entries, atomically.
    pub async fn rename_snapshot(
        &self,
        path: &str,
        name: &str,
        new_name: &str,
        recursive: bool,
    ) -> Result<()> {
        let ids = self
            .storage
            .change_snapshots(path, name, recursive, "rename", Some(new_name))
            .await?;
        for id in ids {
            self.reindex_file_in_search(id).await?;
        }
        Ok(())
    }

    pub(super) async fn copy_snapshot(
        &self,
        source: &str,
        name: &str,
        destination: &str,
        recursive: bool,
    ) -> Result<()> {
        crate::resource_path::canonical_path_segments(destination)?;
        let directory = self.resolve_directory_path(source).await.is_ok();
        if directory && !recursive {
            return Err(Error::InvalidInput(
                "copying a snapshot directory selector requires recursion".into(),
            ));
        }
        let snapshots = self.list_snapshots(source, Some(name), recursive).await?;
        if snapshots.is_empty() {
            return Err(Error::NotFound(format!("{source}@{name}")));
        }
        let destination_is_dir = self.resolve_directory_path(destination).await.is_ok();
        let source_name = source.rsplit('/').next().unwrap_or("");
        let root = if destination_is_dir {
            format!("{}/{source_name}", destination.trim_end_matches('/'))
        } else {
            destination.to_string()
        };
        if directory && (root == source || root.starts_with(&format!("{source}/"))) {
            return Err(Error::InvalidInput(
                "cannot copy a snapshot subtree into itself".into(),
            ));
        }
        for snapshot in snapshots {
            let live_path = snapshot.canonical_path().split_once('@').unwrap().0;
            let target = if directory {
                format!(
                    "{root}/{}",
                    live_path
                        .strip_prefix(source.trim_end_matches('/'))
                        .unwrap()
                        .trim_start_matches('/')
                )
            } else {
                root.clone()
            };
            let (parent, _) = crate::resource_path::parent_and_final_name(&target)?;
            self.storage.ensure_directory_path(&parent).await?;
            if self.storage.resolve_path(&target, None).await.is_ok() {
                return Err(Error::Duplicate(target));
            }
            self.copy_file_content(snapshot.id(), &target).await?;
            let description = self
                .storage
                .entry_description(snapshot.canonical_path())
                .await?;
            self.set_entry_description(&target, description.as_deref())
                .await?;
            self.set_document_metadata(
                &target,
                &self.storage.file_metadata(snapshot.id()).await?,
                None,
            )
            .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::db::{SqliteDatabase, Storage};
    #[tokio::test]
    async fn snapshots_preserve_text_identity_and_rollback() {
        let tmp = tempfile::tempdir().unwrap();
        let db = SqliteDatabase::init(
            format!("sqlite://{}", tmp.path().join("db").display()),
            tmp.path().join("idx"),
            10,
        )
        .await
        .unwrap();
        let source = db.storage.create_file("/note", "before").await.unwrap();
        let first = db.create_snapshot("/note", "first", false).await.unwrap()[0].id();
        assert_ne!(source, first);
        let other = db.create_snapshot("/note", "other", false).await.unwrap()[0].id();
        db.update_document(source, "after").await.unwrap();
        assert_eq!(db.get_document(first).await.unwrap(), "before");
        assert_eq!(db.get_document(other).await.unwrap(), "before");
        let latest = db.create_snapshot("/note", "latest", false).await.unwrap()[0].id();
        db.rollback_snapshot("/note", "first", false).await.unwrap();
        assert_eq!(db.get_document(source).await.unwrap(), "before");
        assert_eq!(db.get_document(latest).await.unwrap(), "after");
        assert_eq!(db.resolve_file_path("/note").await.unwrap(), source);
        assert_eq!(db.resolve_file_path("/note@first").await.unwrap(), first);
        assert!(db.delete_document(source).await.is_err());
        db.rename_snapshot("/note", "first", "renamed", false)
            .await
            .unwrap();
        assert_eq!(db.resolve_file_path("/note@renamed").await.unwrap(), first);
        db.delete_snapshot("/note", "renamed", false).await.unwrap();
        assert!(db.get_document(first).await.is_err());
        assert_eq!(db.get_document(other).await.unwrap(), "before");
    }

    #[tokio::test]
    async fn recursive_create_conflict_is_atomic() {
        let tmp = tempfile::tempdir().unwrap();
        let db = SqliteDatabase::init(
            format!("sqlite://{}", tmp.path().join("db").display()),
            tmp.path().join("idx"),
            0,
        )
        .await
        .unwrap();
        db.storage
            .create_directory("/dir", None, false)
            .await
            .unwrap();
        db.storage.create_file("/dir/a", "a").await.unwrap();
        db.storage.create_file("/dir/b", "b").await.unwrap();
        db.create_snapshot("/dir/b", "same", false).await.unwrap();
        assert!(db.cp("/dir@same", "/copy", false).await.is_err());
        assert!(db.resolve_directory_path("/copy").await.is_err());

        assert!(db.create_snapshot("/dir", "same", true).await.is_err());
        assert!(
            db.list_snapshots("/dir/a", None, false)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            db.list_snapshots("/dir", None, true).await.unwrap().len(),
            1
        );
    }
}
