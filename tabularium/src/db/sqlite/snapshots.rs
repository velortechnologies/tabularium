//! Versioned snapshot storage. Snapshot identity never contains a source path.
use crate::{Error, Result};
use sqlx::{Row, SqlitePool};

const SCHEMA_VERSION: i64 = 1;

#[allow(clippy::too_many_lines)] // Keep the cohesive operation/scenario together.
pub(super) async fn migrate(pool: &SqlitePool) -> Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut *tx)
        .await?;
    if version > SCHEMA_VERSION {
        return Err(Error::InvalidInput(format!(
            "database schema {version} is newer than supported {SCHEMA_VERSION}"
        )));
    }
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    let rows = sqlx::query(
        "SELECT id, parent_id, name FROM entries WHERE instr(name, char(64)) > 0 ORDER BY id",
    )
    .fetch_all(&mut *tx)
    .await?;
    for row in rows {
        let id: i64 = row.try_get("id")?;
        let parent: i64 = row.try_get("parent_id")?;
        let old: String = row.try_get("name")?;
        let mut name = old.replace('@', "_");
        loop {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM entries WHERE parent_id = ? AND name = ?)",
            )
            .bind(parent)
            .bind(&name)
            .fetch_one(&mut *tx)
            .await?;
            if !exists {
                break;
            }
            name.push('_');
        }
        sqlx::query("UPDATE entries SET name = ? WHERE id = ?")
            .bind(&name)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tracing::info!(entry_id = id, old_name = %old, new_name = %name, "snapshot namespace migration");
    }
    // Snapshots reserve IDs from the live entries sequence, avoiding collisions.
    sqlx::raw_sql(r"
CREATE TABLE snapshot_contents (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content TEXT NOT NULL
);
CREATE TABLE snapshots (
    id INTEGER PRIMARY KEY CHECK(id > 0),
    source_id INTEGER NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    captured_at INTEGER NOT NULL,
    description TEXT,
    metadata_json TEXT NOT NULL,
    mime_type TEXT,
    size INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    modified_at INTEGER NOT NULL,
    revision TEXT NOT NULL,
    content_kind INTEGER NOT NULL,
    sha256 TEXT,
    blob_revision TEXT,
    content_id INTEGER REFERENCES snapshot_contents(id),
    UNIQUE(source_id, name)
);
CREATE TABLE blob_gc (source_id INTEGER NOT NULL, generation TEXT NOT NULL, PRIMARY KEY(source_id, generation));
CREATE TRIGGER snapshot_blob_gc AFTER DELETE ON snapshots
WHEN OLD.blob_revision IS NOT NULL
BEGIN
 INSERT OR IGNORE INTO blob_gc VALUES(OLD.source_id, OLD.blob_revision);
END;
CREATE TRIGGER entry_blob_gc_update AFTER UPDATE OF blob_revision ON entries
WHEN OLD.blob_revision IS NOT NULL AND OLD.blob_revision IS NOT NEW.blob_revision
BEGIN
 INSERT OR IGNORE INTO blob_gc VALUES(OLD.id, OLD.blob_revision);
END;
CREATE TRIGGER entry_blob_gc_delete AFTER DELETE ON entries
WHEN OLD.blob_revision IS NOT NULL
BEGIN
 INSERT OR IGNORE INTO blob_gc VALUES(OLD.id, OLD.blob_revision);
END;
CREATE TABLE storage_state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
INSERT INTO storage_state VALUES ('snapshot_reindex', 'required');
CREATE TRIGGER snapshot_preserve_text BEFORE UPDATE OF content, content_kind ON entries
WHEN OLD.kind = 1 AND OLD.content_kind = 0
 AND (NEW.content IS NOT OLD.content OR NEW.content_kind != OLD.content_kind)
 AND EXISTS(SELECT 1 FROM snapshots WHERE source_id = OLD.id AND content_kind = 0 AND content_id IS NULL)
BEGIN
 INSERT INTO snapshot_contents(content) VALUES (OLD.content);
 UPDATE snapshots SET content_id = last_insert_rowid()
 WHERE source_id = OLD.id AND content_kind = 0 AND content_id IS NULL;
END;
CREATE TRIGGER snapshot_content_cleanup AFTER DELETE ON snapshots
WHEN OLD.content_id IS NOT NULL
BEGIN
 DELETE FROM snapshot_contents WHERE id = OLD.content_id
 AND NOT EXISTS(SELECT 1 FROM snapshots WHERE content_id = OLD.content_id);
END;
CREATE VIEW readable_entries AS
SELECT id, parent_id, kind, name, description, metadata_json, mime_type, content, size,
created_at, modified_at, accessed_at, revision, content_kind, sha256, blob_revision,
id AS source_id, NULL AS snapshot_name
FROM entries
UNION ALL
SELECT s.id, e.parent_id, 1, e.name || char(64) || s.name, s.description, s.metadata_json,
s.mime_type, CASE WHEN s.content_kind = 1 THEN '' ELSE coalesce(c.content, e.content) END,
s.size, s.created_at, s.modified_at, s.captured_at, s.revision, s.content_kind, s.sha256,
s.blob_revision, s.source_id, s.name
FROM snapshots s JOIN entries e ON e.id = s.source_id
LEFT JOIN snapshot_contents c ON c.id = s.content_id;
PRAGMA user_version = 1;
").execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

impl super::SqliteStorage {
    /// Replay committed deletions after a crash, with a fresh reference check.
    pub(super) async fn recover_blob_cleanup(&self) -> Result<()> {
        let pending: Vec<(i64, String)> =
            sqlx::query_as("SELECT source_id, generation FROM blob_gc")
                .fetch_all(&self.pool)
                .await?;
        for (source, generation) in pending {
            self.remove_unreferenced_blob(source.into(), &generation)
                .await?;
        }
        Ok(())
    }

    pub(super) async fn is_snapshot(&self, id: super::EntryId) -> Result<bool> {
        Ok(
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM snapshots WHERE id = ?)")
                .bind(id.raw())
                .fetch_one(&self.pool)
                .await?,
        )
    }

    // Serialize liveness check with snapshot/rollback writes and binary opens.
    pub(super) async fn remove_unreferenced_blob(
        &self,
        source: super::EntryId,
        generation: &str,
    ) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let used: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM snapshots WHERE source_id = ? AND blob_revision = ?) OR EXISTS(SELECT 1 FROM entries WHERE id = ? AND blob_revision = ?)")
            .bind(source.raw()).bind(generation).bind(source.raw()).bind(generation).fetch_one(&mut *tx).await?;
        if !used {
            self.blobs.remove(source, generation)?;
        }
        sqlx::query("DELETE FROM blob_gc WHERE source_id = ? AND generation = ?")
            .bind(source.raw())
            .bind(generation)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

use super::{EntryId, SqliteStorage};
use crate::Timestamp;
use crate::validation::validate_snapshot_name;

/// Query membership inside the same transaction as the operation.
async fn targets(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    path: &str,
    recursive: bool,
) -> Result<Vec<i64>> {
    let segments = crate::resource_path::canonical_path_segments(path)?;
    let mut id = 1i64;
    let mut kind = 0i64;
    for segment in segments {
        let row: Option<(i64, i64)> =
            sqlx::query_as("SELECT id, kind FROM entries WHERE parent_id = ? AND name = ?")
                .bind(id)
                .bind(segment)
                .fetch_optional(&mut **tx)
                .await?;
        (id, kind) = row.ok_or_else(|| Error::NotFound(path.into()))?;
    }
    if kind == 1 {
        return Ok(vec![id]);
    }
    if !recursive {
        return Err(Error::InvalidInput(
            "directory snapshot operation requires recursion".into(),
        ));
    }
    Ok(sqlx::query_scalar("WITH RECURSIVE sub(id) AS (SELECT ? UNION ALL SELECT e.id FROM entries e JOIN sub ON e.parent_id = sub.id WHERE e.id != e.parent_id) SELECT e.id FROM entries e JOIN sub ON e.id = sub.id WHERE e.kind = 1 ORDER BY e.id")
       .bind(id).fetch_all(&mut **tx).await?)
}

impl SqliteStorage {
    pub(super) async fn create_snapshots_impl(
        &self,
        path: &str,
        name: &str,
        recursive: bool,
    ) -> Result<Vec<EntryId>> {
        validate_snapshot_name(name)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let sources = targets(&mut tx, path, recursive).await?;
        let mut ids = Vec::new();
        for source in sources {
            let id: i64 = sqlx::query_scalar(
                "UPDATE sqlite_sequence SET seq = seq + 1 WHERE name = 'entries' RETURNING seq",
            )
            .fetch_one(&mut *tx)
            .await?;
            sqlx::query("INSERT INTO snapshots(id, source_id, name, captured_at, description, metadata_json, mime_type, size, created_at, modified_at, revision, content_kind, sha256, blob_revision) SELECT ?, id, ?, ?, description, metadata_json, mime_type, size, created_at, modified_at, revision, content_kind, sha256, blob_revision FROM entries WHERE id = ?")
                .bind(id).bind(name).bind(Timestamp::now()).bind(source).execute(&mut *tx).await.map_err(Self::map_sqlite_constraint)?;
            ids.push(id.into());
        }
        tx.commit().await?;
        Ok(ids)
    }

    pub(super) async fn snapshot_ids_impl(
        &self,
        path: &str,
        name: Option<&str>,
        recursive: bool,
    ) -> Result<Vec<EntryId>> {
        if let Some(name) = name {
            validate_snapshot_name(name)?;
        }
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let source_ids = match targets(&mut tx, path, recursive).await {
            Ok(ids) => ids,
            Err(Error::InvalidInput(_)) if !recursive => {
                let segs = crate::resource_path::canonical_path_segments(path)?;
                let mut parent = 1i64;
                for segment in segs {
                    parent = sqlx::query_scalar(
                        "SELECT id FROM entries WHERE parent_id = ? AND name = ? AND kind = 0",
                    )
                    .bind(parent)
                    .bind(segment)
                    .fetch_one(&mut *tx)
                    .await?;
                }
                sqlx::query_scalar("SELECT id FROM entries WHERE parent_id = ? AND kind = 1")
                    .bind(parent)
                    .fetch_all(&mut *tx)
                    .await?
            }
            Err(e) => return Err(e),
        };
        let json =
            serde_json::to_string(&source_ids).map_err(|e| Error::InvalidInput(e.to_string()))?;
        let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM snapshots WHERE source_id IN (SELECT value FROM json_each(?)) AND (? IS NULL OR name = ?) ORDER BY captured_at, id")
            .bind(json).bind(name).bind(name).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        Ok(ids.into_iter().map(EntryId::from).collect())
    }

    pub(super) async fn change_snapshots_impl(
        &self,
        path: &str,
        name: &str,
        recursive: bool,
        action: &str,
        new_name: Option<&str>,
    ) -> Result<Vec<EntryId>> {
        validate_snapshot_name(name)?;
        if let Some(n) = new_name {
            validate_snapshot_name(n)?;
        }
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let sources = targets(&mut tx, path, recursive).await?;
        let json =
            serde_json::to_string(&sources).map_err(|e| Error::InvalidInput(e.to_string()))?;
        let rows = sqlx::query("SELECT id, source_id, blob_revision FROM snapshots WHERE source_id IN (SELECT value FROM json_each(?)) AND name = ? ORDER BY id")
            .bind(json).bind(name).fetch_all(&mut *tx).await?;
        if rows.is_empty() {
            return Err(Error::NotFound(format!("{path}@{name}")));
        }
        let mut affected = Vec::new();
        let mut cleanup = Vec::new();
        for row in rows {
            let id: i64 = row.try_get("id")?;
            let source: i64 = row.try_get("source_id")?;
            match action {
                "rename" => {
                    let new_name = new_name
                        .ok_or_else(|| Error::InvalidInput("missing snapshot name".into()))?;
                    sqlx::query("UPDATE snapshots SET name = ? WHERE id = ?")
                        .bind(new_name)
                        .bind(id)
                        .execute(&mut *tx)
                        .await
                        .map_err(Self::map_sqlite_constraint)?;
                    affected.push(id.into());
                }
                "delete" => {
                    let blob: Option<String> = row.try_get("blob_revision")?;
                    if let Some(blob) = blob {
                        cleanup.push((EntryId::from(source), blob));
                    }
                    sqlx::query("DELETE FROM snapshots WHERE id = ?")
                        .bind(id)
                        .execute(&mut *tx)
                        .await?;
                    affected.push(id.into());
                }
                "rollback" => {
                    let old: Option<String> =
                        sqlx::query_scalar("SELECT blob_revision FROM entries WHERE id = ?")
                            .bind(source)
                            .fetch_one(&mut *tx)
                            .await?;
                    // One UPDATE fires text preservation before replacing the live state.
                    sqlx::query("UPDATE entries SET (description, metadata_json, mime_type, content, size, created_at, content_kind, sha256, blob_revision) = (SELECT description, metadata_json, mime_type, content, size, created_at, content_kind, sha256, blob_revision FROM readable_entries WHERE id = ?), modified_at = ?, revision = ? WHERE id = ?")
                        .bind(id).bind(Timestamp::now()).bind(super::new_file_revision()).bind(source).execute(&mut *tx).await?;
                    if let Some(blob) = old {
                        cleanup.push((EntryId::from(source), blob));
                    }
                    affected.push(source.into());
                }
                _ => return Err(Error::InvalidInput("unknown snapshot operation".into())),
            }
        }
        tx.commit().await?;
        for (source, blob) in cleanup {
            self.remove_unreferenced_blob(source, &blob).await?;
        }
        Ok(affected)
    }
}

impl SqliteStorage {
    pub(crate) async fn snapshot_reindex_required(&self) -> Result<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM storage_state WHERE key = 'snapshot_reindex')",
        )
        .fetch_one(&self.pool)
        .await?)
    }
    pub(crate) async fn snapshot_reindex_finished(&self) -> Result<()> {
        sqlx::query("DELETE FROM storage_state WHERE key = 'snapshot_reindex'")
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    use crate::db::Storage;
    #[tokio::test]
    async fn migration_is_versioned_and_renames_only_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", tmp.path().join("legacy").display());
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(
                url.parse::<sqlx::sqlite::SqliteConnectOptions>()
                    .unwrap()
                    .create_if_missing(true)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        super::super::migrate(&pool).await.unwrap();
        for name in ["a_b", "a@b", "a_b_"] {
            sqlx::query("INSERT INTO entries(parent_id,kind,name,content,size,created_at,modified_at,accessed_at,revision) VALUES(1,1,?,'body',4,0,0,0,'r')")
                .bind(name).execute(&pool).await.unwrap();
        }
        migrate(&pool).await.unwrap();
        let names: Vec<String> =
            sqlx::query_scalar("SELECT name FROM entries WHERE kind=1 ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(names, ["a_b", "a_b__", "a_b_"]);
        migrate(&pool).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA user_version")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        sqlx::query("PRAGMA user_version = 999")
            .execute(&pool)
            .await
            .unwrap();
        assert!(migrate(&pool).await.is_err());
    }

    #[tokio::test]
    async fn preserved_text_is_shared_and_reclaimed() {
        let tmp = tempfile::tempdir().unwrap();
        let storage =
            SqliteStorage::connect(format!("sqlite://{}", tmp.path().join("db").display()))
                .await
                .unwrap();
        let id = storage.create_file("/doc", "old").await.unwrap();
        storage
            .create_snapshots_impl("/doc", "a", false)
            .await
            .unwrap();
        storage
            .create_snapshots_impl("/doc", "b", false)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM snapshot_contents")
                .fetch_one(&storage.pool)
                .await
                .unwrap(),
            0
        );
        storage.update_file(id, "new").await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM snapshot_contents")
                .fetch_one(&storage.pool)
                .await
                .unwrap(),
            1
        );
        storage
            .change_snapshots_impl("/doc", "a", false, "delete", None)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM snapshot_contents")
                .fetch_one(&storage.pool)
                .await
                .unwrap(),
            1
        );
        storage
            .change_snapshots_impl("/doc", "b", false, "delete", None)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM snapshot_contents")
                .fetch_one(&storage.pool)
                .await
                .unwrap(),
            0
        );
    }
    #[tokio::test]
    async fn binary_snapshot_restart_rollback_and_last_reference_cleanup() {
        use std::io::Read;
        let tmp = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", tmp.path().join("state.db").display());
        let storage = SqliteStorage::connect(&url).await.unwrap();
        let upload = tmp.path().join("upload");
        std::fs::write(&upload, [0, 255, 42]).unwrap();
        let source = storage
            .put_binary_file("/blob", &upload, "application/octet-stream", 3, "old", None)
            .await
            .unwrap();
        let a = storage
            .create_snapshots_impl("/blob", "a", false)
            .await
            .unwrap()[0];
        let b = storage
            .create_snapshots_impl("/blob", "b", false)
            .await
            .unwrap()[0];
        let old = storage.open_binary_file(a).await.unwrap().0;
        let generation: String =
            sqlx::query_scalar("SELECT blob_revision FROM snapshots WHERE id = ?")
                .bind(a.raw())
                .fetch_one(&storage.pool)
                .await
                .unwrap();
        let old_path = storage.blobs.path(source, &generation).unwrap();
        std::fs::write(&upload, [7, 8]).unwrap();
        storage
            .put_binary_file("/blob", &upload, "application/octet-stream", 2, "new", None)
            .await
            .unwrap();
        assert!(old_path.exists());
        let c = storage
            .create_snapshots_impl("/blob", "c", false)
            .await
            .unwrap()[0];
        drop(storage);
        let storage = SqliteStorage::connect(&url).await.unwrap();
        storage
            .change_snapshots_impl("/blob", "a", false, "rollback", None)
            .await
            .unwrap();
        let (live, mut reader) = storage.open_binary_file(source).await.unwrap();
        assert_eq!(live.id(), source);
        assert_ne!(live.revision(), old.revision());
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, [0, 255, 42]);
        storage
            .change_snapshots_impl("/blob", "a", false, "delete", None)
            .await
            .unwrap();
        storage
            .change_snapshots_impl("/blob", "b", false, "delete", None)
            .await
            .unwrap();
        assert!(
            old_path.exists(),
            "live entry still references old generation"
        );
        assert!(storage.open_binary_file(a).await.is_err());
        assert!(storage.open_binary_file(b).await.is_err());
        storage
            .change_snapshots_impl("/blob", "c", false, "rollback", None)
            .await
            .unwrap();
        assert!(!old_path.exists(), "last old reference has gone");
        assert!(storage.open_binary_file(c).await.is_ok());
        storage.delete_file_recursive(source).await.unwrap();
        let leaf = tmp.path().join("state.blobs/000/000");
        assert_eq!(std::fs::read_dir(leaf).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn concurrent_snapshot_and_write_capture_a_complete_state() {
        let tmp = tempfile::tempdir().unwrap();
        let storage =
            SqliteStorage::connect(format!("sqlite://{}", tmp.path().join("db").display()))
                .await
                .unwrap();
        let source = storage.create_file("/doc", "").await.unwrap();
        for i in 0..12 {
            let name = format!("s{i}");
            let body = "x".repeat(i * 100);
            let (snapshot, write) = tokio::join!(
                storage.create_snapshots_impl("/doc", &name, false),
                storage.update_file(source, &body)
            );
            write.unwrap();
            let id = snapshot.unwrap()[0];
            let captured = storage.get_file_content(id).await.unwrap();
            storage.update_file(source, "next").await.unwrap();
            assert_eq!(storage.get_file_content(id).await.unwrap(), captured);
            assert!(captured == body || captured == "next" || captured.is_empty());
        }
    }
    #[tokio::test]
    async fn committed_blob_cleanup_is_replayed_after_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", tmp.path().join("db").display());
        let storage = SqliteStorage::connect(&url).await.unwrap();
        let upload = tmp.path().join("upload");
        std::fs::write(&upload, b"old").unwrap();
        let source = storage
            .put_binary_file("/blob", &upload, "application/octet-stream", 3, "old", None)
            .await
            .unwrap();
        storage
            .create_snapshots_impl("/blob", "old", false)
            .await
            .unwrap();
        let generation: String = sqlx::query_scalar("SELECT blob_revision FROM entries WHERE id=?")
            .bind(source.raw())
            .fetch_one(&storage.pool)
            .await
            .unwrap();
        let path = storage.blobs.path(source, &generation).unwrap();
        // Abort leaves both the reference and the bytes intact.
        let mut tx = storage.pool.begin().await.unwrap();
        sqlx::query("DELETE FROM entries WHERE id=?")
            .bind(source.raw())
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        assert!(path.exists());
        assert!(storage.open_binary_file(source).await.is_ok());
        // Simulate process death between the committed SQL delete and filesystem cleanup.
        sqlx::query("DELETE FROM entries WHERE id=?")
            .bind(source.raw())
            .execute(&storage.pool)
            .await
            .unwrap();
        assert!(path.exists());
        storage.pool.close().await;
        drop(storage);
        let reopened = SqliteStorage::connect(&url).await.unwrap();
        assert!(!path.exists());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM blob_gc")
                .fetch_one(&reopened.pool)
                .await
                .unwrap(),
            0
        );
    }
}
