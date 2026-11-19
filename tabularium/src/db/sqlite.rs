//! SQLite-backed `Storage` — the first forge of the Tabularium.

use async_trait::async_trait;
use bma_ts::Timestamp;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow};
use sqlx::{QueryBuilder, Row, SqlitePool};
use tracing::instrument;
use uuid::Uuid;

use super::EntryId;
use super::entry_kind::EntryKind;
use super::meta::{DocumentMeta, ListedEntry};
use super::storage::Storage;
use crate::resource_path::{canonical_path_segments, parent_and_final_name};
use crate::validation::validate_entity_name;
use crate::{Error, Result};

mod snapshots;

const ROOT_ID: i64 = 1;

async fn table_exists(pool: &SqlitePool, name: &str) -> Result<bool> {
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?")
            .bind(name)
            .fetch_one(pool)
            .await?;
    Ok(n > 0)
}

fn new_file_revision() -> String {
    Uuid::new_v4().to_string()
}

async fn ensure_revision_column(pool: &SqlitePool) -> Result<()> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('entries') WHERE name = 'revision'",
    )
    .fetch_one(pool)
    .await?;
    if n == 0 {
        sqlx::query("ALTER TABLE entries ADD COLUMN revision TEXT")
            .execute(pool)
            .await?;
    }
    backfill_null_file_revisions(pool).await
}

async fn backfill_null_file_revisions(pool: &SqlitePool) -> Result<()> {
    let ids: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM entries WHERE kind = 1 AND revision IS NULL")
            .fetch_all(pool)
            .await?;
    for id in ids {
        let u = new_file_revision();
        sqlx::query("UPDATE entries SET revision = ? WHERE id = ? AND kind = 1")
            .bind(&u)
            .bind(id)
            .execute(pool)
            .await?;
    }
    Ok(())
}

async fn access_keys_has_name_column(pool: &SqlitePool) -> Result<bool> {
    if !table_exists(pool, "access_keys").await? {
        return Ok(false);
    }
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('access_keys') WHERE name = 'name'",
    )
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

/// Stage-1 testing: if `access_keys` predates the `name` column, drop and recreate empty (no legacy PSK migration).
async fn ensure_access_keys_named_schema(pool: &SqlitePool) -> Result<()> {
    if !table_exists(pool, "acls").await? {
        return Ok(());
    }
    if !table_exists(pool, "access_keys").await? {
        sqlx::query(
            r"CREATE TABLE access_keys (
            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
            name TEXT NOT NULL UNIQUE,
            key TEXT NOT NULL UNIQUE,
            acl_id INTEGER NOT NULL REFERENCES acls(id) ON DELETE CASCADE
        )",
        )
        .execute(pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_access_keys_acl ON access_keys(acl_id)")
            .execute(pool)
            .await?;
        return Ok(());
    }
    if access_keys_has_name_column(pool).await? {
        return Ok(());
    }
    sqlx::query("DROP TABLE access_keys").execute(pool).await?;
    sqlx::query(
        r"CREATE TABLE access_keys (
            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
            name TEXT NOT NULL UNIQUE,
            key TEXT NOT NULL UNIQUE,
            acl_id INTEGER NOT NULL REFERENCES acls(id) ON DELETE CASCADE
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_access_keys_acl ON access_keys(acl_id)")
        .execute(pool)
        .await?;
    Ok(())
}

async fn ensure_acl_tables(pool: &SqlitePool) -> Result<()> {
    if table_exists(pool, "acls").await? {
        return Ok(());
    }
    sqlx::query(
        r"CREATE TABLE acls (
            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
            name TEXT NOT NULL UNIQUE,
            body_json TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r"CREATE TABLE access_keys (
            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
            name TEXT NOT NULL UNIQUE,
            key TEXT NOT NULL UNIQUE,
            acl_id INTEGER NOT NULL REFERENCES acls(id) ON DELETE CASCADE
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_access_keys_acl ON access_keys(acl_id)")
        .execute(pool)
        .await?;
    Ok(())
}

/// Serialize the check and ALTER across simultaneous server starts. Never ignore DDL errors.
async fn ensure_metadata_column(pool: &SqlitePool) -> Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('entries') WHERE name = 'metadata_json'",
    )
    .fetch_one(&mut *tx)
    .await?;
    if n == 0 {
        sqlx::query("ALTER TABLE entries ADD COLUMN metadata_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(metadata_json) AND json_type(metadata_json) = 'object')")
            .execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Add binary metadata without rewriting existing content, timestamps or revisions.
/// blob_revision is independent: metadata edits and moves bump entry revision.
async fn ensure_binary_columns(pool: &SqlitePool) -> Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    for (name, ddl) in [
        (
            "content_kind",
            "ALTER TABLE entries ADD COLUMN content_kind INTEGER NOT NULL DEFAULT 0 CHECK(content_kind IN (0, 1))",
        ),
        ("sha256", "ALTER TABLE entries ADD COLUMN sha256 TEXT"),
        (
            "blob_revision",
            "ALTER TABLE entries ADD COLUMN blob_revision TEXT",
        ),
    ] {
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM pragma_table_info('entries') WHERE name = ?")
                .bind(name)
                .fetch_one(&mut *tx)
                .await?;
        if n == 0 {
            sqlx::query(ddl).execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

async fn migrate(pool: &SqlitePool) -> Result<()> {
    if table_exists(pool, "entries").await? {
        ensure_revision_column(pool).await?;
        ensure_metadata_column(pool).await?;
        ensure_binary_columns(pool).await?;
        ensure_acl_tables(pool).await?;
        ensure_access_keys_named_schema(pool).await?;
        return Ok(());
    }
    sqlx::query(
        r"CREATE TABLE entries (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    parent_id INTEGER NOT NULL REFERENCES entries(id) ON DELETE RESTRICT,
    kind INTEGER NOT NULL,
    name TEXT NOT NULL,
    description TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(metadata_json) AND json_type(metadata_json) = 'object'),
    mime_type TEXT DEFAULT NULL,
    content TEXT,
    size INTEGER,
    created_at INTEGER NOT NULL,
    modified_at INTEGER NOT NULL,
    accessed_at INTEGER NOT NULL,
    revision TEXT,
    UNIQUE(parent_id, name),
    CHECK(parent_id != id OR id = 1),
    CHECK((kind = 0 AND content IS NULL AND size IS NULL) OR (kind = 1 AND content IS NOT NULL AND size IS NOT NULL))
)",
    )
    .execute(pool)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_entries_parent ON entries(parent_id)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_entries_kind ON entries(kind)")
        .execute(pool)
        .await?;
    let now = Timestamp::now();
    sqlx::query(
        r"INSERT INTO entries (id, parent_id, kind, name, description, content, size, created_at, modified_at, accessed_at, revision)
         VALUES (1, 1, 0, '', NULL, NULL, NULL, ?, ?, ?, NULL)",
    )
    .bind(now)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;
    ensure_binary_columns(pool).await?;
    ensure_acl_tables(pool).await?;
    ensure_access_keys_named_schema(pool).await?;
    Ok(())
}

/// Derived state is in the source database so invalidation commits with mutations.
async fn indexing_flags_schema(pool: &SqlitePool) -> Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::raw_sql(r"
CREATE TABLE IF NOT EXISTS entry_index_state (
 id INTEGER PRIMARY KEY,
 generation INTEGER NOT NULL DEFAULT 0,
 lexical_indexed INTEGER NOT NULL DEFAULT 0 CHECK(lexical_indexed IN (0,1)),
 semantic_indexed INTEGER NOT NULL DEFAULT 0 CHECK(semantic_indexed IN (0,1))
);
CREATE INDEX IF NOT EXISTS idx_pending_lexical ON entry_index_state(id) WHERE lexical_indexed=0;
CREATE INDEX IF NOT EXISTS idx_pending_semantic ON entry_index_state(id) WHERE semantic_indexed=0;
INSERT OR IGNORE INTO entry_index_state(id) SELECT id FROM entries WHERE kind=1;
INSERT OR IGNORE INTO entry_index_state(id) SELECT id FROM snapshots;
CREATE TRIGGER IF NOT EXISTS index_entry_insert AFTER INSERT ON entries WHEN NEW.kind=1
BEGIN INSERT INTO entry_index_state(id) VALUES(NEW.id); END;
CREATE TRIGGER IF NOT EXISTS index_snapshot_insert AFTER INSERT ON snapshots
BEGIN INSERT INTO entry_index_state(id) VALUES(NEW.id); END;
CREATE TRIGGER IF NOT EXISTS index_entry_delete AFTER DELETE ON entries
BEGIN DELETE FROM entry_index_state WHERE id=OLD.id; END;
CREATE TRIGGER IF NOT EXISTS index_snapshot_delete AFTER DELETE ON snapshots
BEGIN DELETE FROM entry_index_state WHERE id=OLD.id; END;
CREATE TRIGGER IF NOT EXISTS index_entry_change
AFTER UPDATE OF revision,content,metadata_json,description,name,parent_id,content_kind ON entries
BEGIN
 UPDATE entry_index_state SET generation=generation+1,lexical_indexed=0,semantic_indexed=0
 WHERE id=NEW.id OR ((NEW.name IS NOT OLD.name OR NEW.parent_id IS NOT OLD.parent_id) AND id IN (
 WITH RECURSIVE subtree(id) AS (
 SELECT NEW.id UNION ALL SELECT e.id FROM entries e JOIN subtree s ON e.parent_id=s.id WHERE e.id!=e.parent_id
 ) SELECT id FROM subtree UNION ALL SELECT id FROM snapshots WHERE source_id IN (SELECT id FROM subtree)
 ));
END;
CREATE TRIGGER IF NOT EXISTS index_snapshot_change
AFTER UPDATE OF revision,metadata_json,description,name,content_kind ON snapshots
BEGIN UPDATE entry_index_state SET generation=generation+1,lexical_indexed=0,semantic_indexed=0 WHERE id=NEW.id; END;
").execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

fn index_candidates_sql(kind: crate::SearchKind, missing: bool, semantic: bool) -> String {
    let flags = match (missing, kind, semantic) {
        (false, _, _) => "SELECT * FROM entry_index_state",
        (true, crate::SearchKind::Lexical, _) | (true, crate::SearchKind::Hybrid, false) => {
            "SELECT * FROM entry_index_state INDEXED BY idx_pending_lexical WHERE lexical_indexed=0"
        }
        (true, crate::SearchKind::Semantic, true) => {
            "SELECT * FROM entry_index_state INDEXED BY idx_pending_semantic WHERE semantic_indexed=0"
        }
        (true, crate::SearchKind::Semantic, false) => "SELECT * FROM entry_index_state WHERE 0",
        (true, crate::SearchKind::Hybrid, true) => {
            "SELECT * FROM entry_index_state INDEXED BY idx_pending_lexical WHERE lexical_indexed=0 UNION SELECT * FROM entry_index_state INDEXED BY idx_pending_semantic WHERE semantic_indexed=0"
        }
    };
    let eligibility = if missing && kind == crate::SearchKind::Semantic {
        "d.content_kind=0"
    } else if missing && kind == crate::SearchKind::Hybrid && semantic {
        "(f.lexical_indexed=0 OR d.content_kind=0)"
    } else {
        "1"
    };
    // No readable_entries view here: snapshot bodies must never be joined/read.
    format!(
        r"
WITH RECURSIVE flags AS MATERIALIZED ({flags}), dirs(id,path) AS (
 SELECT ?, ? UNION ALL
 SELECT e.id, rtrim(dirs.path,'/') || '/' || e.name FROM entries e JOIN dirs ON e.parent_id=dirs.id
 WHERE e.kind=0 AND e.id!=e.parent_id
), documents AS (
 SELECT e.id, rtrim(dirs.path,'/') || '/' || e.name AS path,e.content_kind
 FROM entries e JOIN dirs ON e.parent_id=dirs.id WHERE e.kind=1
 UNION ALL
 SELECT s.id, rtrim(dirs.path,'/') || '/' || e.name || '@' || s.name,s.content_kind
 FROM snapshots s JOIN entries e ON e.id=s.source_id JOIN dirs ON dirs.id=e.parent_id
)
SELECT d.id,d.path,f.generation,f.lexical_indexed,f.semantic_indexed,d.content_kind
FROM flags f JOIN documents d ON d.id=f.id WHERE {eligibility} ORDER BY d.id"
    )
}

/// SQLite implementation of [`Storage`].
pub struct SqliteStorage {
    #[cfg(test)]
    pub(super) metadata_reads: std::sync::atomic::AtomicUsize,
    pool: SqlitePool,
    blobs: super::blob::BlobStore,
}

impl SqliteStorage {
    pub fn set_blob_root(&mut self, root: std::path::PathBuf) {
        self.blobs = super::blob::BlobStore::new(root);
    }
    pub fn new_binary_upload(&self) -> Result<tempfile::NamedTempFile> {
        self.blobs.temporary()
    }
    pub async fn create_browser_session(&self, key: &str) -> Result<String> {
        use sha2::{Digest, Sha256};
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let hash = format!("{:x}", Sha256::digest(token.as_bytes()));
        let result = sqlx::query("INSERT INTO browser_sessions (token_hash, psk_id) SELECT ?, id FROM access_keys WHERE key = ?")
            .bind(hash).bind(key).execute(&self.pool).await?;
        if result.rows_affected() != 1 {
            return Err(Error::Unauthorized("unknown key".into()));
        }
        Ok(token)
    }
    pub async fn browser_session_acl(&self, token: &str) -> Result<Option<String>> {
        use sha2::{Digest, Sha256};
        let hash = format!("{:x}", Sha256::digest(token.as_bytes()));
        Ok(sqlx::query_scalar("SELECT a.name FROM browser_sessions s JOIN access_keys p ON p.id = s.psk_id JOIN acls a ON a.id = p.acl_id WHERE s.token_hash = ?")
            .bind(hash).fetch_optional(&self.pool).await?)
    }
    pub async fn delete_browser_session(&self, token: &str) -> Result<()> {
        use sha2::{Digest, Sha256};
        let hash = format!("{:x}", Sha256::digest(token.as_bytes()));
        sqlx::query("DELETE FROM browser_sessions WHERE token_hash = ?")
            .bind(hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Connects (creating the database file if needed) and applies DDL.
    #[instrument(name = "sqlite_storage_connect", fields(db_uri = db_uri.as_ref()), err(Debug))]
    pub async fn connect(db_uri: impl AsRef<str>) -> Result<Self> {
        let opts = db_uri
            .as_ref()
            .parse::<SqliteConnectOptions>()
            .map_err(|e| Error::InvalidInput(e.to_string()))?
            .create_if_missing(true)
            .foreign_keys(true);
        let blob_root = opts.get_filename().with_extension("blobs");
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await?;
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&pool)
            .await?;
        if version > 1 {
            return Err(Error::InvalidInput(format!(
                "unsupported database schema {version}"
            )));
        }
        migrate(&pool).await?;
        snapshots::migrate(&pool).await?;
        indexing_flags_schema(&pool).await?;
        sqlx::query("CREATE TABLE IF NOT EXISTS browser_sessions (token_hash TEXT PRIMARY KEY, psk_id INTEGER NOT NULL REFERENCES access_keys(id) ON DELETE CASCADE)")
            .execute(&pool).await?;
        let storage = Self {
            #[cfg(test)]
            metadata_reads: std::sync::atomic::AtomicUsize::new(0),
            pool,
            blobs: super::blob::BlobStore::new(blob_root),
        };
        storage.recover_blob_cleanup().await?;
        Ok(storage)
    }

    async fn text_write_failure(&self, file_id: EntryId) -> Result<()> {
        let kind: Option<i64> =
            sqlx::query_scalar("SELECT content_kind FROM entries WHERE id = ? AND kind = 1")
                .bind(file_id.raw())
                .fetch_optional(&self.pool)
                .await?;
        match kind {
            Some(1) => Err(Error::BinaryEntry(file_id.to_string())),
            Some(_) => Err(Error::RevisionMismatch(file_id.to_string())),
            None => Err(Error::NotFound(file_id.to_string())),
        }
    }

    fn map_sqlite_constraint(err: sqlx::Error) -> Error {
        if let Some(db) = err.as_database_error() {
            let msg = db.message();
            if msg.contains("UNIQUE") {
                return Error::Duplicate("name already exists in directory".into());
            }
            if msg.contains("FOREIGN KEY") || msg.contains("RESTRICT") {
                return Error::InvalidInput("reference conflict".into());
            }
        }
        err.into()
    }

    fn map_dir_delete_fk(err: sqlx::Error) -> Error {
        if let Some(db) = err.as_database_error()
            && (db.message().contains("FOREIGN KEY") || db.message().contains("RESTRICT"))
        {
            return Error::NotEmpty("directory is not empty".into());
        }
        err.into()
    }

    /// ACL rows `(name, body_json)` for operator rites.
    #[instrument(name = "sqlite_acl_list", skip(self), err(Debug))]
    pub async fn acl_list_rows(&self) -> Result<Vec<(String, String)>> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT name, body_json FROM acls ORDER BY name ASC")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows)
    }

    #[instrument(name = "sqlite_acl_get", skip(self), fields(name = %name), err(Debug))]
    pub async fn acl_get_json(&self, name: &str) -> Result<String> {
        let row: Option<(String,)> = sqlx::query_as("SELECT body_json FROM acls WHERE name = ?")
            .bind(name)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|t| t.0)
            .ok_or_else(|| Error::NotFound(format!("acl `{name}`")))
    }

    #[instrument(name = "sqlite_acl_upsert", skip(self, body_json), fields(name = %name), err(Debug))]
    pub async fn acl_upsert_validated(&self, name: &str, body_json: &str) -> Result<()> {
        validate_entity_name(name)?;
        crate::parse_acl_json(body_json)?;
        sqlx::query(
            r"INSERT INTO acls (name, body_json) VALUES (?, ?)
              ON CONFLICT(name) DO UPDATE SET body_json = excluded.body_json",
        )
        .bind(name)
        .bind(body_json)
        .execute(&self.pool)
        .await
        .map_err(Self::map_sqlite_constraint)?;
        Ok(())
    }

    #[instrument(name = "sqlite_acl_delete", skip(self), fields(name = %name), err(Debug))]
    pub async fn acl_delete_named(&self, name: &str) -> Result<()> {
        let r = sqlx::query("DELETE FROM acls WHERE name = ?")
            .bind(name)
            .execute(&self.pool)
            .await?;
        if r.rows_affected() == 0 {
            return Err(Error::NotFound(format!("acl `{name}`")));
        }
        Ok(())
    }

    #[instrument(name = "sqlite_auth_lookup_key", skip(self, key), err(Debug))]
    pub async fn auth_lookup_key(&self, key: &str) -> Result<Option<(String, String)>> {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT a.name, a.body_json FROM access_keys k JOIN acls a ON k.acl_id = a.id WHERE k.key = ?",
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    #[instrument(name = "sqlite_auth_lookup_acl_named", skip(self, name), err(Debug))]
    pub async fn auth_lookup_acl_named(&self, name: &str) -> Result<Option<(String, String)>> {
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT name, body_json FROM acls WHERE name = ?")
                .bind(name)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row)
    }

    #[instrument(name = "sqlite_psk_list", skip(self), err(Debug))]
    pub async fn psk_list_rows(&self) -> Result<Vec<(String, String, String)>> {
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT k.name, a.name, k.key FROM access_keys k JOIN acls a ON k.acl_id = a.id ORDER BY a.name ASC, k.name ASC, k.key ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    #[instrument(name = "sqlite_psk_insert", skip(self, key), fields(psk_name = %psk_name, acl_name = %acl_name), err(Debug))]
    pub async fn psk_insert(&self, psk_name: &str, acl_name: &str, key: &str) -> Result<()> {
        validate_entity_name(psk_name)?;
        let acl_id: Option<i64> = sqlx::query_scalar("SELECT id FROM acls WHERE name = ?")
            .bind(acl_name)
            .fetch_optional(&self.pool)
            .await?;
        let Some(aid) = acl_id else {
            return Err(Error::NotFound(format!("acl `{acl_name}`")));
        };
        sqlx::query("INSERT INTO access_keys (name, key, acl_id) VALUES (?, ?, ?)")
            .bind(psk_name)
            .bind(key)
            .bind(aid)
            .execute(&self.pool)
            .await
            .map_err(Self::map_sqlite_constraint)?;
        Ok(())
    }

    #[instrument(name = "sqlite_psk_delete", skip(self), fields(psk_name = %psk_name), err(Debug))]
    pub async fn psk_delete_named(&self, psk_name: &str) -> Result<()> {
        let r = sqlx::query("DELETE FROM access_keys WHERE name = ?")
            .bind(psk_name)
            .execute(&self.pool)
            .await?;
        if r.rows_affected() == 0 {
            return Err(Error::NotFound(format!("psk `{psk_name}`")));
        }
        Ok(())
    }
}

/// JSON array of path segments for [`WITH RECURSIVE`] resolution (Ferrum gate: one CTE walk).
pub(crate) fn path_segments_json(segments: &[String]) -> Result<String> {
    serde_json::to_string(segments).map_err(|e| Error::InvalidInput(e.to_string()))
}

async fn resolve_path_id(
    pool: &SqlitePool,
    segments: &[String],
    expected_kind: Option<EntryKind>,
) -> Result<EntryId> {
    if segments.is_empty() {
        let id = EntryId::from_raw(ROOT_ID);
        if let Some(k) = expected_kind {
            let row_kind: EntryKind = sqlx::query_scalar("SELECT kind FROM entries WHERE id = ?")
                .bind(ROOT_ID)
                .fetch_one(pool)
                .await?;
            if row_kind != k {
                return Err(Error::NotFound("entry /".into()));
            }
        }
        return Ok(id);
    }
    let json = path_segments_json(segments)?;
    let id: Option<i64> = sqlx::query_scalar(
        r"
        WITH RECURSIVE
        segs(idx, seg_name) AS (
            SELECT CAST(key AS INTEGER), value FROM json_each(?1)
        ),
        walk(cur_id, ri) AS (
            SELECT 1, -1
            UNION ALL
            SELECT e.id, s.idx
            FROM walk w
            JOIN segs s ON s.idx = w.ri + 1
            JOIN entries e ON e.parent_id = w.cur_id AND e.name = s.seg_name
        )
        SELECT cur_id FROM walk WHERE ri = (SELECT MAX(idx) FROM segs)
        ",
    )
    .bind(&json)
    .fetch_optional(pool)
    .await?;
    let Some(raw) = id else {
        let p = format!("/{}", segments.join("/"));
        return Err(Error::NotFound(p));
    };
    if let Some(k) = expected_kind {
        let row_kind: EntryKind = sqlx::query_scalar("SELECT kind FROM entries WHERE id = ?")
            .bind(raw)
            .fetch_one(pool)
            .await?;
        if row_kind != k {
            let p = format!("/{}", segments.join("/"));
            return Err(Error::NotFound(p));
        }
    }
    Ok(EntryId::from_raw(raw))
}

async fn canonical_path_for_id(pool: &SqlitePool, entry_id: EntryId) -> Result<String> {
    if entry_id.raw() == ROOT_ID {
        return Ok("/".to_string());
    }
    let names: Vec<String> = sqlx::query_scalar(
        r"
        WITH RECURSIVE up AS (
            SELECT id, parent_id, name, 0 AS lvl FROM readable_entries WHERE id = ?
            UNION ALL
            SELECT e.id, e.parent_id, e.name, up.lvl + 1
            FROM entries e
            INNER JOIN up ON e.id = up.parent_id
            WHERE up.parent_id != up.id
        )
        SELECT name FROM up ORDER BY lvl ASC
        ",
    )
    .bind(entry_id.raw())
    .fetch_all(pool)
    .await?;
    if names.is_empty() {
        return Err(Error::NotFound(format!("entry {}", entry_id.raw())));
    }
    let mut parts: Vec<&str> = names.iter().map(String::as_str).collect();
    parts.reverse();
    if parts.first().is_some_and(|s| s.is_empty()) {
        parts.remove(0);
    }
    if parts.is_empty() {
        return Ok("/".to_string());
    }
    Ok(format!("/{}", parts.join("/")))
}

async fn recursive_file_count_under(pool: &SqlitePool, dir_id: i64) -> Result<u64> {
    let n: i64 = sqlx::query_scalar(
        r"
        WITH RECURSIVE sub AS (
            SELECT id FROM entries WHERE id = ?
            UNION ALL
            SELECT e.id FROM entries e
            JOIN sub ON e.parent_id = sub.id
            WHERE e.id != e.parent_id
        )
        SELECT COUNT(*) FROM entries WHERE kind = 1 AND id IN (SELECT id FROM sub)
        ",
    )
    .bind(dir_id)
    .fetch_one(pool)
    .await?;
    u64::try_from(n).map_err(|_| Error::InvalidInput("file count overflow".into()))
}

fn row_to_file_meta(row: &SqliteRow, canonical_path: String) -> Result<DocumentMeta> {
    Ok(DocumentMeta::new(
        EntryId::from_raw(row.try_get::<i64, _>(0)?),
        EntryId::from_raw(row.try_get::<i64, _>(1)?),
        row.try_get::<String, _>(2)?,
        canonical_path,
        row.try_get::<Timestamp, _>(3)?,
        row.try_get::<Timestamp, _>(4)?,
        row.try_get::<Timestamp, _>(5)?,
        row.try_get::<Option<String>, _>(6)?,
        row.try_get::<i64, _>(7)?,
    )
    .with_mime_type(row.try_get::<Option<String>, _>(8)?)
    .with_binary_metadata(
        u8::try_from(row.try_get::<i64, _>(9)?)
            .map_err(|_| Error::InvalidInput("invalid binary metadata version".into()))?,
        row.try_get(10)?,
    ))
}

#[derive(Clone, Copy)]
enum MkdirParentsScan {
    Dir(EntryId),
    Missing,
}

#[async_trait]
impl Storage for SqliteStorage {
    async fn index_candidates(
        &self,
        path: &str,
        kind: crate::SearchKind,
        missing: bool,
        semantic: bool,
    ) -> Result<Vec<(EntryId, String, i64, bool, bool, bool)>> {
        let root = self.resolve_path(path, Some(EntryKind::Dir)).await?;
        let query = index_candidates_sql(kind, missing, semantic);
        let rows: Vec<(i64, String, i64, bool, bool, bool)> = sqlx::query_as(&query)
            .bind(root.raw())
            .bind(path)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|(id, p, g, l, s, b)| (EntryId::from_raw(id), p, g, l, s, b))
            .collect())
    }
    async fn index_generation(&self, id: EntryId) -> Result<(i64, bool, bool)> {
        sqlx::query_as(
            "SELECT generation,lexical_indexed,semantic_indexed FROM entry_index_state WHERE id=?",
        )
        .bind(id.raw())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| Error::NotFound(id.to_string()))
    }
    async fn mark_indexed(&self, id: EntryId, generation: i64, semantic: bool) -> Result<()> {
        let query = if semantic {
            "UPDATE entry_index_state SET semantic_indexed=1 WHERE id=? AND generation=?"
        } else {
            "UPDATE entry_index_state SET lexical_indexed=1 WHERE id=? AND generation=?"
        };
        sqlx::query(query)
            .bind(id.raw())
            .bind(generation)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    async fn prepare_index(&self, id: EntryId, semantic: bool) -> Result<()> {
        let query = if semantic {
            "UPDATE entry_index_state SET semantic_indexed=0 WHERE id=?"
        } else {
            "UPDATE entry_index_state SET lexical_indexed=0 WHERE id=?"
        };
        sqlx::query(query)
            .bind(id.raw())
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    async fn sync_index_identity(&self, semantic: bool, identity: &str) -> Result<()> {
        let key = if semantic {
            "semantic_identity"
        } else {
            "lexical_identity"
        };
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let old: Option<String> = sqlx::query_scalar("SELECT value FROM storage_state WHERE key=?")
            .bind(key)
            .fetch_optional(&mut *tx)
            .await?;
        if old.as_deref() != Some(identity) {
            let query = if semantic {
                "UPDATE entry_index_state SET semantic_indexed=0,generation=generation+1"
            } else {
                "UPDATE entry_index_state SET lexical_indexed=0,generation=generation+1"
            };
            sqlx::query(query).execute(&mut *tx).await?;
            sqlx::query("INSERT OR REPLACE INTO storage_state(key,value) VALUES(?,?)")
                .bind(key)
                .bind(identity)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn delete_file_recursive(&self, file_id: EntryId) -> Result<Vec<EntryId>> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM snapshots WHERE source_id = ?")
            .bind(file_id.raw())
            .fetch_all(&mut *tx)
            .await?;
        let blobs: Vec<String> = sqlx::query_scalar("SELECT blob_revision FROM snapshots WHERE source_id = ? AND blob_revision IS NOT NULL UNION SELECT blob_revision FROM entries WHERE id = ? AND blob_revision IS NOT NULL")
            .bind(file_id.raw()).bind(file_id.raw()).fetch_all(&mut *tx).await?;
        let changed = sqlx::query("DELETE FROM entries WHERE id = ? AND kind = 1")
            .bind(file_id.raw())
            .execute(&mut *tx)
            .await?;
        if changed.rows_affected() == 0 {
            return Err(Error::NotFound(file_id.to_string()));
        }
        tx.commit().await?;
        for blob in blobs {
            self.remove_unreferenced_blob(file_id, &blob).await?;
        }
        Ok(ids.into_iter().map(EntryId::from).collect())
    }

    async fn create_snapshots(
        &self,
        path: &str,
        name: &str,
        recursive: bool,
    ) -> Result<Vec<EntryId>> {
        self.create_snapshots_impl(path, name, recursive).await
    }
    async fn snapshot_ids(
        &self,
        path: &str,
        name: Option<&str>,
        recursive: bool,
    ) -> Result<Vec<EntryId>> {
        self.snapshot_ids_impl(path, name, recursive).await
    }
    async fn change_snapshots(
        &self,
        path: &str,
        name: &str,
        recursive: bool,
        action: &str,
        new_name: Option<&str>,
    ) -> Result<Vec<EntryId>> {
        self.change_snapshots_impl(path, name, recursive, action, new_name)
            .await
    }

    async fn replace_binary_with_text(
        &self,
        file_id: EntryId,
        content: &str,
        mime: &str,
        expected_revision: Option<&str>,
    ) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let old: Option<(Option<String>, Option<String>)> =
            sqlx::query_as("SELECT revision, blob_revision FROM entries WHERE id = ? AND kind = 1")
                .bind(file_id.raw())
                .fetch_optional(&mut *tx)
                .await?;
        let Some((revision, generation)) = old else {
            return Err(Error::NotFound(file_id.to_string()));
        };
        if expected_revision == Some("*") {
            return Err(Error::Duplicate(file_id.to_string()));
        }
        if expected_revision.is_some_and(|v| Some(v) != revision.as_deref()) {
            return Err(Error::RevisionMismatch(file_id.to_string()));
        }
        let size = i64::try_from(content.len())
            .map_err(|_| Error::InvalidInput("size overflow".into()))?;
        sqlx::query("UPDATE entries SET content_kind = 0, content = ?, size = ?, mime_type = ?, sha256 = NULL, blob_revision = NULL, revision = ?, modified_at = ? WHERE id = ?")
            .bind(content).bind(size).bind(mime).bind(new_file_revision()).bind(Timestamp::now()).bind(file_id.raw())
            .execute(&mut *tx).await?;
        tx.commit().await?;
        if let Some(generation) = generation {
            self.remove_unreferenced_blob(file_id, &generation).await?;
        }
        Ok(())
    }

    fn new_binary_upload(&self) -> Result<tempfile::NamedTempFile> {
        self.blobs.temporary()
    }
    async fn put_binary_file(
        &self,
        path: &str,
        temporary: &std::path::Path,
        mime: &str,
        size: i64,
        sha256: &str,
        expected_revision: Option<&str>,
    ) -> Result<EntryId> {
        let (parent, name) = parent_and_final_name(path)?;
        let parent_id = self.resolve_path(&parent, Some(EntryKind::Dir)).await?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let old: Option<(i64, i64, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT id, kind, revision, blob_revision FROM entries WHERE parent_id = ? AND name = ?")
            .bind(parent_id.raw()).bind(&name).fetch_optional(&mut *tx).await?;
        let revision = new_file_revision();
        let now = Timestamp::now();
        let (id, old_generation) = if let Some((id, kind, old_revision, old_generation)) = old {
            if expected_revision == Some("*") {
                return Err(Error::Duplicate(path.into()));
            }
            if kind != 1 {
                return Err(Error::Duplicate(path.into()));
            }
            if expected_revision.is_some_and(|expected| Some(expected) != old_revision.as_deref()) {
                return Err(Error::RevisionMismatch(path.into()));
            }
            (id, old_generation)
        } else {
            if expected_revision.is_some_and(|r| r != "*") {
                return Err(Error::NotFound(path.into()));
            }
            let id: i64 = sqlx::query_scalar("INSERT INTO entries (parent_id, kind, name, content, size, created_at, modified_at, accessed_at, revision) VALUES (?, 1, ?, '', 0, ?, ?, ?, ?) RETURNING id")
                .bind(parent_id.raw()).bind(&name).bind(now).bind(now).bind(now).bind(&revision)
                .fetch_one(&mut *tx).await?;
            (id, None)
        };
        self.blobs.publish(temporary, id.into(), &revision)?;
        let result = sqlx::query("UPDATE entries SET content_kind = 1, content = '', mime_type = ?, size = ?, sha256 = ?, blob_revision = ?, revision = ?, modified_at = ? WHERE id = ?")
            .bind(mime).bind(size).bind(sha256).bind(&revision).bind(&revision).bind(now).bind(id)
            .execute(&mut *tx).await;
        if let Err(e) = result {
            tx.rollback().await?;
            self.blobs.remove(id.into(), &revision)?;
            return Err(e.into());
        }
        // On uncertain commit failure retain the new generation; never remove a potentially live blob.
        tx.commit().await?;
        if let Some(old) = old_generation {
            self.remove_unreferenced_blob(id.into(), &old).await?;
        }
        Ok(id.into())
    }

    async fn open_binary_file(&self, file_id: EntryId) -> Result<(DocumentMeta, std::fs::File)> {
        // Serialize open with replacement/delete until the OS file handle is acquired.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let generation: Option<Option<String>> = sqlx::query_scalar(
            "SELECT blob_revision FROM readable_entries WHERE id = ? AND content_kind = 1",
        )
        .bind(file_id.raw())
        .fetch_optional(&mut *tx)
        .await?;
        let generation = generation
            .flatten()
            .ok_or_else(|| Error::NotFound(file_id.to_string()))?;
        let row = sqlx::query("SELECT id, parent_id, name, created_at, modified_at, accessed_at, revision, size, mime_type, content_kind, sha256 FROM readable_entries WHERE id = ?")
            .bind(file_id.raw()).fetch_one(&mut *tx).await?;
        let source: i64 = sqlx::query_scalar("SELECT source_id FROM readable_entries WHERE id = ?")
            .bind(file_id.raw())
            .fetch_one(&mut *tx)
            .await?;
        let file = std::fs::File::open(self.blobs.path(source.into(), &generation)?)?;
        let path = canonical_path_for_id(&self.pool, file_id).await?;
        let meta = row_to_file_meta(&row, path)?;
        tx.commit().await?;
        Ok((meta, file))
    }

    async fn file_metadata(&self, file_id: EntryId) -> Result<serde_json::Value> {
        let raw: Option<String> = sqlx::query_scalar(
            "SELECT metadata_json FROM readable_entries WHERE id = ? AND kind = 1",
        )
        .bind(file_id.raw())
        .fetch_optional(&self.pool)
        .await?;
        let raw = raw.ok_or_else(|| Error::NotFound(format!("file {}", file_id.raw())))?;
        serde_json::from_str(&raw).map_err(|e| Error::InvalidInput(format!("stored metadata: {e}")))
    }

    async fn set_file_metadata(
        &self,
        file_id: EntryId,
        metadata: &str,
        expected_revision: Option<&str>,
    ) -> Result<()> {
        let result = sqlx::query("UPDATE entries SET metadata_json = ?, modified_at = ?, revision = ? WHERE id = ? AND kind = 1 AND (? IS NULL OR revision = ?)")
            .bind(metadata).bind(Timestamp::now()).bind(new_file_revision()).bind(file_id.raw())
            .bind(expected_revision).bind(expected_revision).execute(&self.pool).await?;
        if result.rows_affected() == 0 {
            self.get_file_meta(file_id).await?;
            return Err(Error::RevisionMismatch(format!("file {}", file_id.raw())));
        }
        Ok(())
    }

    #[instrument(skip(self), fields(file_id = file_id.raw()), err(Debug))]
    async fn touch(&self, file_id: EntryId) -> Result<()> {
        if self.is_snapshot(file_id).await? {
            return Ok(());
        }
        let now = Timestamp::now();
        let r = sqlx::query("UPDATE entries SET accessed_at = ? WHERE id = ? AND kind = 1")
            .bind(now)
            .bind(file_id.raw())
            .execute(&self.pool)
            .await?;
        if r.rows_affected() == 0 {
            return Err(Error::NotFound(format!("file {}", file_id.raw())));
        }
        Ok(())
    }

    /// `NotFound` is normal when probing file-vs-directory (REST `get_or_list`); omit `err(Debug)` to avoid ERROR spam.
    #[instrument(skip(self), fields(path = %path, expected_kind = ?expected_kind))]
    async fn resolve_path(&self, path: &str, expected_kind: Option<EntryKind>) -> Result<EntryId> {
        if let Some((source, name)) = crate::resource_path::split_snapshot_path(path)? {
            if expected_kind == Some(EntryKind::Dir) {
                return Err(Error::NotFound(path.into()));
            }
            let segs = canonical_path_segments(source)?;
            let source_id = resolve_path_id(&self.pool, &segs, Some(EntryKind::File)).await?;
            let id: Option<i64> =
                sqlx::query_scalar("SELECT id FROM snapshots WHERE source_id = ? AND name = ?")
                    .bind(source_id.raw())
                    .bind(name)
                    .fetch_optional(&self.pool)
                    .await?;
            return id
                .map(EntryId::from)
                .ok_or_else(|| Error::NotFound(path.into()));
        }
        let segs = canonical_path_segments(path)?;
        resolve_path_id(&self.pool, &segs, expected_kind).await
    }

    #[instrument(skip(self), fields(entry_id = entry_id.raw()), err(Debug))]
    async fn canonical_path(&self, entry_id: EntryId) -> Result<String> {
        canonical_path_for_id(&self.pool, entry_id).await
    }

    #[instrument(skip(self), fields(file_id = file_id.raw()), err(Debug))]
    async fn parent_dir_path_for_file(&self, file_id: EntryId) -> Result<String> {
        let parent: i64 =
            sqlx::query_scalar("SELECT parent_id FROM readable_entries WHERE id = ? AND kind = 1")
                .bind(file_id.raw())
                .fetch_optional(&self.pool)
                .await?
                .ok_or_else(|| Error::NotFound(format!("file {}", file_id.raw())))?;
        canonical_path_for_id(&self.pool, EntryId::from_raw(parent)).await
    }

    #[instrument(
        skip(self, description),
        fields(path = %path, parents = parents),
        err(Debug)
    )]
    async fn create_directory(
        &self,
        path: &str,
        description: Option<&str>,
        parents: bool,
    ) -> Result<EntryId> {
        if !parents {
            let (parent_path, name) = parent_and_final_name(path)?;
            let parent_id = self
                .resolve_path(&parent_path, Some(EntryKind::Dir))
                .await?;
            let now = Timestamp::now();
            let id: i64 = sqlx::query_scalar(
                r"INSERT INTO entries (parent_id, kind, name, description, content, size, created_at, modified_at, accessed_at)
                VALUES (?, 0, ?, ?, NULL, NULL, ?, ?, ?) RETURNING id",
            )
            .bind(parent_id.raw())
            .bind(&name)
            .bind(description)
            .bind(now)
            .bind(now)
            .bind(now)
            .fetch_one(&self.pool)
            .await
            .map_err(Self::map_sqlite_constraint)?;
            return Ok(EntryId::from_raw(id));
        }

        let segs = canonical_path_segments(path)?;
        if segs.is_empty() {
            return Err(Error::InvalidInput(
                "path must name an entry under root (not / alone)".into(),
            ));
        }

        let mut cur_id = EntryId::from_raw(ROOT_ID);
        let mut states: Vec<MkdirParentsScan> = Vec::with_capacity(segs.len());

        for (i, seg) in segs.iter().enumerate() {
            let row: Option<(i64, i64)> =
                sqlx::query_as("SELECT id, kind FROM entries WHERE parent_id = ? AND name = ?")
                    .bind(cur_id.raw())
                    .bind(seg)
                    .fetch_optional(&self.pool)
                    .await?;

            match row {
                None => {
                    states.push(MkdirParentsScan::Missing);
                    for _ in (i + 1)..segs.len() {
                        states.push(MkdirParentsScan::Missing);
                    }
                    break;
                }
                Some((id, kind)) => {
                    if kind == EntryKind::File as i64 {
                        return Err(Error::InvalidInput(format!(
                            "create_directory: path segment names an existing file ({seg})"
                        )));
                    }
                    cur_id = EntryId::from_raw(id);
                    states.push(MkdirParentsScan::Dir(cur_id));
                }
            }
        }

        if states.len() == segs.len()
            && let MkdirParentsScan::Dir(leaf_id) = states[segs.len() - 1]
        {
            return Ok(leaf_id);
        }

        let first_missing = states
            .iter()
            .position(|s| matches!(s, MkdirParentsScan::Missing))
            .expect("parents mkdir: expected a missing segment");
        let mut cur_id = EntryId::from_raw(ROOT_ID);
        for ent in states.iter().take(first_missing) {
            if let MkdirParentsScan::Dir(id) = *ent {
                cur_id = id;
            }
        }

        for i in first_missing..segs.len() {
            let is_leaf = i + 1 == segs.len();
            let desc = if is_leaf { description } else { None };
            let now = Timestamp::now();
            let id: i64 = sqlx::query_scalar(
                r"INSERT INTO entries (parent_id, kind, name, description, content, size, created_at, modified_at, accessed_at)
                VALUES (?, 0, ?, ?, NULL, NULL, ?, ?, ?) RETURNING id",
            )
            .bind(cur_id.raw())
            .bind(&segs[i])
            .bind(desc)
            .bind(now)
            .bind(now)
            .bind(now)
            .fetch_one(&self.pool)
            .await
            .map_err(Self::map_sqlite_constraint)?;
            cur_id = EntryId::from_raw(id);
        }
        Ok(cur_id)
    }

    #[instrument(skip(self), fields(path = %path), err(Debug))]
    async fn delete_directory(&self, path: &str) -> Result<()> {
        let id = self.resolve_path(path, Some(EntryKind::Dir)).await?;
        if id.raw() == ROOT_ID {
            return Err(Error::InvalidInput("cannot delete root".into()));
        }
        let r = sqlx::query("DELETE FROM entries WHERE id = ? AND kind = 0")
            .bind(id.raw())
            .execute(&self.pool)
            .await
            .map_err(Self::map_dir_delete_fk)?;
        if r.rows_affected() == 0 {
            return Err(Error::NotFound(path.to_string()));
        }
        Ok(())
    }

    #[instrument(skip(self), fields(path = %path), err(Debug))]
    async fn delete_directory_recursive(&self, path: &str) -> Result<()> {
        let id = self.resolve_path(path, Some(EntryKind::Dir)).await?;
        if id.raw() == ROOT_ID {
            return Err(Error::InvalidInput("cannot delete root".into()));
        }
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let ids: Vec<i64> = sqlx::query_scalar("WITH RECURSIVE sub AS (SELECT id, 0 AS depth FROM entries WHERE id = ? UNION ALL SELECT e.id, sub.depth + 1 FROM entries e JOIN sub ON e.parent_id = sub.id WHERE e.id != e.parent_id) SELECT id FROM sub ORDER BY depth DESC")
            .bind(id.raw()).fetch_all(&mut *tx).await?;
        let mut blobs = Vec::new();
        for eid in ids {
            let historical: Vec<String> = sqlx::query_scalar("SELECT DISTINCT blob_revision FROM snapshots WHERE source_id = ? AND blob_revision IS NOT NULL")
                .bind(eid).fetch_all(&mut *tx).await?;
            blobs.extend(historical.into_iter().map(|generation| (eid, generation)));
            let generation: Option<String> =
                sqlx::query_scalar("SELECT blob_revision FROM entries WHERE id = ?")
                    .bind(eid)
                    .fetch_one(&mut *tx)
                    .await?;
            if let Some(generation) = generation {
                blobs.push((eid, generation));
            }
            sqlx::query("DELETE FROM entries WHERE id = ?")
                .bind(eid)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        for (id, generation) in blobs {
            self.remove_unreferenced_blob(id.into(), &generation)
                .await?;
        }
        Ok(())
    }

    #[instrument(skip(self), fields(old_path = %old_path, new_path = %new_path), err(Debug))]
    async fn rename_directory(&self, old_path: &str, new_path: &str) -> Result<()> {
        let (p_old, name_old) = parent_and_final_name(old_path)?;
        let (p_new, name_new) = parent_and_final_name(new_path)?;
        if p_old != p_new {
            return Err(Error::InvalidInput(
                "rename_directory: parent must match (use move for relocation)".into(),
            ));
        }
        let id = self.resolve_path(old_path, Some(EntryKind::Dir)).await?;
        if id.raw() == ROOT_ID {
            return Err(Error::InvalidInput("cannot rename root".into()));
        }
        let now = Timestamp::now();
        let r =
            sqlx::query("UPDATE entries SET name = ?, modified_at = ? WHERE id = ? AND kind = 0")
                .bind(&name_new)
                .bind(now)
                .bind(id.raw())
                .execute(&self.pool)
                .await
                .map_err(Self::map_sqlite_constraint)?;
        if r.rows_affected() == 0 {
            return Err(Error::NotFound(old_path.to_string()));
        }
        let _ = name_old;
        Ok(())
    }

    #[instrument(skip(self, content), fields(path = %path), err(Debug))]
    async fn create_file(&self, path: &str, content: impl AsRef<str> + Send) -> Result<EntryId> {
        let (parent_path, name) = parent_and_final_name(path)?;
        let parent_id = self
            .resolve_path(&parent_path, Some(EntryKind::Dir))
            .await?;
        let now = Timestamp::now();
        let rev = new_file_revision();
        let size = i64::try_from(content.as_ref().len())
            .map_err(|_| Error::InvalidInput("file content size overflow".into()))?;
        let id: i64 = sqlx::query_scalar(
            r"INSERT INTO entries (parent_id, kind, name, description, content, size, created_at, modified_at, accessed_at, revision)
            VALUES (?, 1, ?, NULL, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(parent_id.raw())
        .bind(&name)
        .bind(content.as_ref())
        .bind(size)
        .bind(now)
        .bind(now)
        .bind(now)
        .bind(&rev)
        .fetch_one(&self.pool)
        .await
        .map_err(Self::map_sqlite_constraint)?;
        Ok(EntryId::from_raw(id))
    }

    #[instrument(skip(self), fields(file_id = file_id.raw()), err(Debug))]
    async fn delete_file(&self, file_id: EntryId) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let generation: Option<Option<String>> =
            sqlx::query_scalar("SELECT blob_revision FROM entries WHERE id = ? AND kind = 1")
                .bind(file_id.raw())
                .fetch_optional(&mut *tx)
                .await?;
        let Some(generation) = generation else {
            return Err(Error::NotFound(file_id.to_string()));
        };
        let has_snapshots: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM snapshots WHERE source_id = ?)")
                .bind(file_id.raw())
                .fetch_one(&mut *tx)
                .await?;
        if has_snapshots {
            return Err(Error::NotEmpty(
                "Cannot delete document: it has snapshots. Delete the snapshots first, or use tb rm -r <path> to delete the document and ALL its snapshots.".into(),
            ));
        }
        sqlx::query("DELETE FROM entries WHERE id = ?")
            .bind(file_id.raw())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        if let Some(generation) = generation {
            self.remove_unreferenced_blob(file_id, &generation).await?;
        }
        Ok(())
    }

    #[instrument(skip(self, new_content), fields(file_id = file_id.raw()), err(Debug))]
    async fn update_file(
        &self,
        file_id: EntryId,
        new_content: impl AsRef<str> + Send,
    ) -> Result<()> {
        let now = Timestamp::now();
        let rev = new_file_revision();
        let size = i64::try_from(new_content.as_ref().len())
            .map_err(|_| Error::InvalidInput("file content size overflow".into()))?;
        let r = sqlx::query(
            "UPDATE entries SET content = ?, size = ?, modified_at = ?, revision = ? WHERE id = ? AND kind = 1 AND content_kind = 0",
        )
        .bind(new_content.as_ref())
        .bind(size)
        .bind(now)
        .bind(&rev)
        .bind(file_id.raw())
        .execute(&self.pool)
        .await?;
        if r.rows_affected() == 0 {
            return self.text_write_failure(file_id).await;
        }
        Ok(())
    }

    #[instrument(skip(self, new_content), fields(file_id = file_id.raw()), err(Debug))]
    async fn update_file_if_revision_matches(
        &self,
        file_id: EntryId,
        new_content: impl AsRef<str> + Send,
        expected_revision: &str,
    ) -> Result<()> {
        let now = Timestamp::now();
        let rev = new_file_revision();
        let size = i64::try_from(new_content.as_ref().len())
            .map_err(|_| Error::InvalidInput("file content size overflow".into()))?;
        let r = sqlx::query(
            "UPDATE entries SET content = ?, size = ?, modified_at = ?, revision = ? WHERE id = ? AND kind = 1 AND content_kind = 0 AND revision = ?",
        )
        .bind(new_content.as_ref())
        .bind(size)
        .bind(now)
        .bind(&rev)
        .bind(file_id.raw())
        .bind(expected_revision)
        .execute(&self.pool)
        .await?;
        if r.rows_affected() > 0 {
            return Ok(());
        }
        self.text_write_failure(file_id).await
    }

    #[instrument(skip(self, to_append), fields(file_id = file_id.raw()), err(Debug))]
    async fn append_file(&self, file_id: EntryId, to_append: impl AsRef<str> + Send) -> Result<()> {
        let now = Timestamp::now();
        let rev = new_file_revision();
        let piece = to_append.as_ref();
        let r = sqlx::query(
            r"UPDATE entries SET
                content = content || CASE WHEN substr(content, -1) = char(10) THEN '' ELSE char(10) END || ?,
                modified_at = ?,
                revision = ?,
                size = length(content || CASE WHEN substr(content, -1) = char(10) THEN '' ELSE char(10) END || ?)
            WHERE id = ? AND kind = 1 AND content_kind = 0",
        )
        .bind(piece)
        .bind(now)
        .bind(&rev)
        .bind(piece)
        .bind(file_id.raw())
        .execute(&self.pool)
        .await?;
        if r.rows_affected() == 0 {
            return self.text_write_failure(file_id).await;
        }
        Ok(())
    }

    #[instrument(skip(self), fields(file_id = file_id.raw()), err(Debug))]
    async fn bump_file_modified_at(&self, file_id: EntryId) -> Result<()> {
        let now = Timestamp::now();
        let rev = new_file_revision();
        let r = sqlx::query(
            "UPDATE entries SET modified_at = ?, revision = ? WHERE id = ? AND kind = 1",
        )
        .bind(now)
        .bind(&rev)
        .bind(file_id.raw())
        .execute(&self.pool)
        .await?;
        if r.rows_affected() == 0 {
            return Err(Error::NotFound(format!("file {}", file_id.raw())));
        }
        Ok(())
    }

    #[instrument(skip(self), fields(entry_id = entry_id.raw()), err(Debug))]
    async fn set_entry_modified_at(&self, entry_id: EntryId, modified_at: Timestamp) -> Result<()> {
        if entry_id.raw() == ROOT_ID {
            return Err(Error::InvalidInput("cannot set modified_at on root".into()));
        }
        let now_rev = new_file_revision();
        let r = sqlx::query(
            "UPDATE entries SET modified_at = ?, revision = CASE WHEN kind = 1 THEN ? ELSE revision END WHERE id = ?",
        )
        .bind(modified_at)
        .bind(&now_rev)
        .bind(entry_id.raw())
        .execute(&self.pool)
        .await?;
        if r.rows_affected() == 0 {
            return Err(Error::NotFound(format!("entry {}", entry_id.raw())));
        }
        Ok(())
    }

    #[instrument(
        skip(self),
        fields(file_id = file_id.raw(), new_parent_path = %new_parent_path, new_name = %new_name),
        err(Debug)
    )]
    async fn move_file(
        &self,
        file_id: EntryId,
        new_parent_path: &str,
        new_name: &str,
    ) -> Result<()> {
        let new_parent = self
            .resolve_path(new_parent_path, Some(EntryKind::Dir))
            .await?;
        let now = Timestamp::now();
        let rev = new_file_revision();
        let r = sqlx::query(
            "UPDATE entries SET parent_id = ?, name = ?, modified_at = ?, revision = ? WHERE id = ? AND kind = 1",
        )
        .bind(new_parent.raw())
        .bind(new_name)
        .bind(now)
        .bind(&rev)
        .bind(file_id.raw())
        .execute(&self.pool)
        .await;
        match r {
            Ok(r) if r.rows_affected() > 0 => Ok(()),
            Ok(_) => Err(Error::NotFound(format!("file {}", file_id.raw()))),
            Err(e) => Err(Self::map_sqlite_constraint(e)),
        }
    }

    #[instrument(skip(self, new_name), fields(file_id = file_id.raw()), err(Debug))]
    async fn rename_file(&self, file_id: EntryId, new_name: impl AsRef<str> + Send) -> Result<()> {
        let now = Timestamp::now();
        let rev = new_file_revision();
        let r = sqlx::query(
            "UPDATE entries SET name = ?, modified_at = ?, revision = ? WHERE id = ? AND kind = 1",
        )
        .bind(new_name.as_ref())
        .bind(now)
        .bind(&rev)
        .bind(file_id.raw())
        .execute(&self.pool)
        .await;
        match r {
            Ok(r) if r.rows_affected() > 0 => Ok(()),
            Ok(_) => Err(Error::NotFound(format!("file {}", file_id.raw()))),
            Err(e) => Err(Self::map_sqlite_constraint(e)),
        }
    }

    #[instrument(skip(self), fields(file_id = file_id.raw()), err(Debug))]
    async fn get_file_content(&self, file_id: EntryId) -> Result<String> {
        let row: Option<(i64, String)> = sqlx::query_as(
            "SELECT content_kind, content FROM readable_entries WHERE id = ? AND kind = 1",
        )
        .bind(file_id.raw())
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some((0, content)) => Ok(content),
            Some(_) => Err(Error::BinaryEntry(file_id.to_string())),
            None => Err(Error::NotFound(file_id.to_string())),
        }
    }

    #[instrument(skip(self), fields(dir_path = %dir_path), err(Debug))]
    async fn list_directory(&self, dir_path: &str) -> Result<Vec<ListedEntry>> {
        let dir_id = self.resolve_path(dir_path, Some(EntryKind::Dir)).await?;
        let rows = sqlx::query(
            r"SELECT id, kind, name, description, created_at, modified_at, accessed_at, revision, size
            FROM entries WHERE parent_id = ? AND id != parent_id ORDER BY kind, name",
        )
        .bind(dir_id.raw())
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let id: i64 = row.try_get(0)?;
            let kind: EntryKind = row.try_get(1)?;
            let name: String = row.try_get(2)?;
            let description: Option<String> = row.try_get(3)?;
            let created_at: Timestamp = row.try_get(4)?;
            let modified_at: Timestamp = row.try_get(5)?;
            let accessed_at: Timestamp = row.try_get(6)?;
            let revision: Option<String> = row.try_get(7)?;
            let size: Option<i64> = row.try_get(8)?;
            let recursive_file_count = if kind == EntryKind::Dir {
                recursive_file_count_under(&self.pool, id).await?
            } else {
                0
            };
            out.push(ListedEntry::new(
                EntryId::from_raw(id),
                kind,
                name,
                description,
                created_at,
                modified_at,
                accessed_at,
                if kind == EntryKind::File {
                    revision.clone()
                } else {
                    None
                },
                if kind == EntryKind::File { size } else { None },
                recursive_file_count,
            ));
        }
        Ok(out)
    }

    #[instrument(skip(self), fields(file_id = file_id.raw()), err(Debug))]
    async fn get_file_meta(&self, file_id: EntryId) -> Result<DocumentMeta> {
        #[cfg(test)]
        self.metadata_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let row = sqlx::query(
            r"SELECT id, parent_id, name, created_at, modified_at, accessed_at, revision, size, mime_type, content_kind, sha256
            FROM readable_entries WHERE id = ? AND kind = 1",
        )
        .bind(file_id.raw())
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Err(Error::NotFound(format!("file {}", file_id.raw())));
        };
        let cp = canonical_path_for_id(&self.pool, file_id).await?;
        row_to_file_meta(&row, cp)
    }

    #[instrument(skip(self, ids), fields(hit_count = ids.len()), err(Debug))]
    async fn files_display_batch(&self, ids: &[EntryId]) -> Result<Vec<(EntryId, String, String)>> {
        const CHUNK: usize = 500;
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(CHUNK) {
            let mut qb =
                QueryBuilder::new("SELECT id FROM readable_entries WHERE kind = 1 AND id IN (");
            {
                let mut sep = qb.separated(", ");
                for id in chunk {
                    sep.push_bind(id.raw());
                }
            }
            qb.push(')');
            let rows = qb.build().fetch_all(&self.pool).await?;
            for row in rows {
                let id = EntryId::from_raw(row.try_get::<i64, _>(0)?);
                let path = canonical_path_for_id(&self.pool, id).await?;
                let content: String =
                    sqlx::query_scalar("SELECT content FROM readable_entries WHERE id = ?")
                        .bind(id.raw())
                        .fetch_one(&self.pool)
                        .await?;
                out.push((id, path, content));
            }
        }
        Ok(out)
    }

    #[instrument(skip(self), fields(file_id = file_id.raw()), err(Debug))]
    async fn file_parent_and_content(&self, file_id: EntryId) -> Result<(EntryId, String)> {
        let row = sqlx::query(
            "SELECT parent_id, content FROM readable_entries WHERE id = ? AND kind = 1",
        )
        .bind(file_id.raw())
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Err(Error::NotFound(format!("file {}", file_id.raw())));
        };
        let pid: i64 = row.try_get(0)?;
        let content: String = row.try_get(1)?;
        Ok((EntryId::from_raw(pid), content))
    }

    #[instrument(skip(self), fields(filter = ?directory_path_filter), err(Debug))]
    async fn files_for_search_reindex(
        &self,
        directory_path_filter: Option<&str>,
    ) -> Result<Vec<(EntryId, String, String, String, String)>> {
        let file_ids: Vec<i64> = match directory_path_filter {
            None => {
                sqlx::query_scalar("SELECT id FROM readable_entries WHERE kind = 1")
                    .fetch_all(&self.pool)
                    .await?
            }
            Some(p) => {
                let dir_id = self.resolve_path(p, Some(EntryKind::Dir)).await?;
                sqlx::query_scalar(
                    r"
                    WITH RECURSIVE sub AS (
                        SELECT id FROM entries WHERE id = ?
                        UNION ALL
                        SELECT e.id FROM entries e
                        JOIN sub ON e.parent_id = sub.id
                        WHERE e.id != e.parent_id
                    )
                    SELECT id FROM readable_entries WHERE kind = 1 AND source_id IN (SELECT id FROM sub)
                    ",
                )
                .bind(dir_id.raw())
                .fetch_all(&self.pool)
                .await?
            }
        };
        let mut out = Vec::with_capacity(file_ids.len());
        for raw in file_ids {
            let id = EntryId::from_raw(raw);
            let parent = self.parent_dir_path_for_file(id).await?;
            let row = sqlx::query(
                "SELECT name, trim(coalesce(description, '') || ' ' || metadata_json), content FROM readable_entries WHERE id = ? AND kind = 1",
            )
            .bind(raw)
            .fetch_one(&self.pool)
            .await?;
            let name: String = row.try_get(0)?;
            let description: Option<String> = row.try_get(1)?;
            let content: String = row.try_get(2)?;
            out.push((id, parent, name, description.unwrap_or_default(), content));
        }
        Ok(out)
    }

    async fn file_search_index_fields(
        &self,
        file_id: EntryId,
    ) -> Result<(String, String, String, String)> {
        let dp = self.parent_dir_path_for_file(file_id).await?;
        let row =
            sqlx::query("SELECT name, trim(coalesce(description, '') || ' ' || metadata_json), content FROM readable_entries WHERE id = ? AND kind = 1")
                .bind(file_id.raw())
                .fetch_optional(&self.pool)
                .await?;
        let Some(row) = row else {
            return Err(Error::NotFound(format!("file {}", file_id.raw())));
        };
        let name: String = row.try_get(0)?;
        let description: Option<String> = row.try_get(1)?;
        let content: String = row.try_get(2)?;
        Ok((dp, name, description.unwrap_or_default(), content))
    }

    #[instrument(
        skip(self),
        fields(dir_id = dir_id.raw(), new_parent_path = %new_parent_path, new_name = %new_name),
        err(Debug)
    )]
    async fn move_directory(
        &self,
        dir_id: EntryId,
        new_parent_path: &str,
        new_name: &str,
    ) -> Result<()> {
        if dir_id.raw() == ROOT_ID {
            return Err(Error::InvalidInput("cannot move root".into()));
        }
        let new_parent = self
            .resolve_path(new_parent_path, Some(EntryKind::Dir))
            .await?;
        let mut tx = self.pool.begin().await?;
        let cycle: i64 = sqlx::query_scalar(
            r"
            WITH RECURSIVE sub AS (
                SELECT id FROM entries WHERE id = ?
                UNION ALL
                SELECT e.id FROM entries e
                JOIN sub ON e.parent_id = sub.id
                WHERE e.id != e.parent_id
            )
            SELECT COUNT(*) FROM sub WHERE id = ?
            ",
        )
        .bind(dir_id.raw())
        .bind(new_parent.raw())
        .fetch_one(&mut *tx)
        .await?;
        if cycle > 0 {
            return Err(Error::InvalidInput(
                "cannot move directory into its own subtree".into(),
            ));
        }
        let now = Timestamp::now();
        let r = sqlx::query(
            "UPDATE entries SET parent_id = ?, name = ?, modified_at = ? WHERE id = ? AND kind = 0",
        )
        .bind(new_parent.raw())
        .bind(new_name)
        .bind(now)
        .bind(dir_id.raw())
        .execute(&mut *tx)
        .await;
        match r {
            Ok(r) if r.rows_affected() > 0 => {}
            Ok(_) => {
                return Err(Error::NotFound(format!("directory {}", dir_id.raw())));
            }
            Err(e) => return Err(Self::map_sqlite_constraint(e)),
        }
        tx.commit().await?;
        Ok(())
    }

    #[instrument(skip(self), fields(dir_path = %dir_path), err(Debug))]
    async fn ensure_directory_path(&self, dir_path: &str) -> Result<EntryId> {
        let segs = canonical_path_segments(dir_path)?;
        let mut cur = EntryId::from_raw(ROOT_ID);
        for seg in segs {
            let row: Option<(i64, i64)> =
                sqlx::query_as("SELECT id, kind FROM entries WHERE parent_id = ? AND name = ?")
                    .bind(cur.raw())
                    .bind(&seg)
                    .fetch_optional(&self.pool)
                    .await?;
            cur = match row {
                None => {
                    let now = Timestamp::now();
                    let id: i64 = sqlx::query_scalar(
                        r"INSERT INTO entries (parent_id, kind, name, description, content, size, created_at, modified_at, accessed_at)
                        VALUES (?, 0, ?, NULL, NULL, NULL, ?, ?, ?) RETURNING id",
                    )
                    .bind(cur.raw())
                    .bind(&seg)
                    .bind(now)
                    .bind(now)
                    .bind(now)
                    .fetch_one(&self.pool)
                    .await
                    .map_err(Self::map_sqlite_constraint)?;
                    EntryId::from_raw(id)
                }
                Some((id, kind)) => {
                    if kind == EntryKind::File as i64 {
                        return Err(Error::InvalidInput(format!(
                            "ensure_directory_path: path segment names an existing file ({seg})"
                        )));
                    }
                    EntryId::from_raw(id)
                }
            };
        }
        Ok(cur)
    }

    #[instrument(skip(self), fields(path = %path), err(Debug))]
    async fn entry_description(&self, path: &str) -> Result<Option<String>> {
        let id = self.resolve_path(path, None).await?;
        let d: Option<String> =
            sqlx::query_scalar("SELECT description FROM readable_entries WHERE id = ?")
                .bind(id.raw())
                .fetch_one(&self.pool)
                .await?;
        Ok(d)
    }

    #[instrument(skip(self, description), fields(path = %path), err(Debug))]
    async fn set_entry_description(&self, path: &str, description: Option<&str>) -> Result<()> {
        let id = self.resolve_path(path, None).await?;
        let now = Timestamp::now();
        let rev = new_file_revision();
        let r = sqlx::query(
            "UPDATE entries SET description = ?, modified_at = ?, revision = CASE WHEN kind = 1 THEN ? ELSE revision END WHERE id = ?",
        )
        .bind(description)
        .bind(now)
        .bind(&rev)
        .bind(id.raw())
        .execute(&self.pool)
        .await?;
        if r.rows_affected() == 0 {
            return Err(Error::NotFound(path.to_string()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn binary_lifecycle_replace_move_metadata_and_delete() {
        use std::io::Read;
        let dir = tempfile::tempdir().unwrap();
        let storage = super::SqliteStorage::connect(format!(
            "sqlite://{}",
            dir.path().join("blobs.db").display()
        ))
        .await
        .unwrap();
        storage
            .create_directory("/media", None, false)
            .await
            .unwrap();
        let temp = dir.path().join("upload");
        std::fs::write(&temp, [0, 255, 1]).unwrap();
        let id = storage
            .put_binary_file("/media/a.png", &temp, "image/png", 3, "test-hash", None)
            .await
            .unwrap();
        let (meta, mut old_reader) = storage.open_binary_file(id).await.unwrap();
        assert!(meta.is_binary());
        storage.set_file_metadata(id, "{}", None).await.unwrap();
        storage.rename_file(id, "renamed.png").await.unwrap();
        let (renamed, _) = storage.open_binary_file(id).await.unwrap();
        assert_eq!(renamed.canonical_path(), "/media/renamed.png");
        std::fs::write(&temp, [9, 8]).unwrap();
        assert!(matches!(
            storage
                .put_binary_file(
                    "/media/renamed.png",
                    &temp,
                    "image/png",
                    2,
                    "new-hash",
                    meta.revision()
                )
                .await,
            Err(crate::Error::RevisionMismatch(_))
        ));
        storage
            .put_binary_file(
                "/media/renamed.png",
                &temp,
                "image/png",
                2,
                "new-hash",
                renamed.revision(),
            )
            .await
            .unwrap();
        let mut bytes = Vec::new();
        old_reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, [0, 255, 1]);
        let (_, mut new_reader) = storage.open_binary_file(id).await.unwrap();
        bytes.clear();
        new_reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, [9, 8]);
        storage.delete_directory_recursive("/media").await.unwrap();
        assert!(storage.open_binary_file(id).await.is_err());
        let leaf = dir.path().join("blobs.blobs/000/000");
        assert_eq!(std::fs::read_dir(leaf).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn binary_text_operations_fail_without_changing_revision_or_size() {
        let dir = tempfile::tempdir().unwrap();
        let storage = super::SqliteStorage::connect(format!(
            "sqlite://{}",
            dir.path().join("guard.db").display()
        ))
        .await
        .unwrap();
        let id = storage.create_file("/picture.png", "").await.unwrap();
        sqlx::query("UPDATE entries SET content_kind = 1, size = 123 WHERE id = ?")
            .bind(id.raw())
            .execute(&storage.pool)
            .await
            .unwrap();
        let before = storage.get_file_meta(id).await.unwrap();
        assert!(matches!(
            storage.get_file_content(id).await,
            Err(crate::Error::BinaryEntry(_))
        ));
        assert!(matches!(
            storage.update_file(id, "corrupt").await,
            Err(crate::Error::BinaryEntry(_))
        ));
        assert!(matches!(
            storage.append_file(id, "corrupt").await,
            Err(crate::Error::BinaryEntry(_))
        ));
        assert!(matches!(
            storage
                .update_file_if_revision_matches(id, "corrupt", before.revision().unwrap())
                .await,
            Err(crate::Error::BinaryEntry(_))
        ));
        let after = storage.get_file_meta(id).await.unwrap();
        assert_eq!(before.revision(), after.revision());
        assert_eq!(after.size_bytes(), 123);
    }

    #[tokio::test]
    async fn binary_upgrade_preserves_legacy_rows_and_is_repeatable() {
        let dir = tempfile::tempdir().unwrap();
        let uri = format!(
            "sqlite://{}",
            dir.path().join("binary-upgrade.db").display()
        );
        let storage = super::SqliteStorage::connect(&uri).await.unwrap();
        let id = storage
            .create_file("/legacy.md", "unchanged UTF-8: пташка")
            .await
            .unwrap();
        let before = storage.get_file_meta(id).await.unwrap();
        // Model a pre-snapshot database before dropping legacy columns.
        sqlx::raw_sql("DROP TRIGGER index_entry_insert; DROP TRIGGER index_entry_change; DROP TRIGGER index_entry_delete; DROP TABLE entry_index_state; DROP VIEW readable_entries; DROP TRIGGER snapshot_preserve_text; DROP TRIGGER snapshot_content_cleanup; DROP TRIGGER snapshot_blob_gc; DROP TRIGGER entry_blob_gc_update; DROP TRIGGER entry_blob_gc_delete; DROP TABLE blob_gc; DROP TABLE snapshots; DROP TABLE snapshot_contents; DROP TABLE storage_state; PRAGMA user_version=0;")
            .execute(&storage.pool).await.unwrap();
        for name in ["content_kind", "sha256", "blob_revision"] {
            sqlx::query(&format!("ALTER TABLE entries DROP COLUMN {name}"))
                .execute(&storage.pool)
                .await
                .unwrap();
        }
        super::migrate(&storage.pool).await.unwrap();
        super::migrate(&storage.pool).await.unwrap();
        super::snapshots::migrate(&storage.pool).await.unwrap();
        super::snapshots::migrate(&storage.pool).await.unwrap();
        assert_eq!(
            storage.get_file_content(id).await.unwrap(),
            "unchanged UTF-8: пташка"
        );
        let after = storage.get_file_meta(id).await.unwrap();
        assert_eq!(before.revision(), after.revision());
        assert_eq!(before.modified_at(), after.modified_at());
        let row: (i64, Option<String>, Option<String>) =
            sqlx::query_as("SELECT content_kind, sha256, blob_revision FROM entries WHERE id = ?")
                .bind(id.raw())
                .fetch_one(&storage.pool)
                .await
                .unwrap();
        assert_eq!(row, (0, None, None));
        let integrity: String = sqlx::query_scalar("PRAGMA quick_check")
            .fetch_one(&storage.pool)
            .await
            .unwrap();
        assert_eq!(integrity, "ok");
    }

    #[tokio::test]
    async fn metadata_upgrade_preserves_legacy_rows_and_is_repeatable() {
        use crate::db::Storage;
        let dir = tempfile::tempdir().unwrap();
        let uri = format!("sqlite://{}", dir.path().join("legacy.db").display());
        let storage = super::SqliteStorage::connect(&uri).await.unwrap();
        let id = storage
            .create_file("/legacy.md", "untouched body")
            .await
            .unwrap();
        let before = storage.get_file_meta(id).await.unwrap();
        // Model a pre-snapshot database before dropping legacy columns.
        sqlx::raw_sql("DROP TRIGGER index_entry_insert; DROP TRIGGER index_entry_change; DROP TRIGGER index_entry_delete; DROP TABLE entry_index_state; DROP VIEW readable_entries; DROP TRIGGER snapshot_preserve_text; DROP TRIGGER snapshot_content_cleanup; DROP TRIGGER snapshot_blob_gc; DROP TRIGGER entry_blob_gc_update; DROP TRIGGER entry_blob_gc_delete; DROP TABLE blob_gc; DROP TABLE snapshots; DROP TABLE snapshot_contents; DROP TABLE storage_state; PRAGMA user_version=0;")
            .execute(&storage.pool).await.unwrap();
        sqlx::query("ALTER TABLE entries DROP COLUMN metadata_json")
            .execute(&storage.pool)
            .await
            .unwrap();
        super::migrate(&storage.pool).await.unwrap();
        super::migrate(&storage.pool).await.unwrap();
        super::snapshots::migrate(&storage.pool).await.unwrap();
        super::snapshots::migrate(&storage.pool).await.unwrap();
        assert_eq!(
            storage.file_metadata(id).await.unwrap(),
            serde_json::json!({})
        );
        assert_eq!(
            storage.get_file_content(id).await.unwrap(),
            "untouched body"
        );
        let after = storage.get_file_meta(id).await.unwrap();
        assert_eq!(before.revision(), after.revision());
        assert_eq!(before.modified_at(), after.modified_at());
        assert!(
            sqlx::query("UPDATE entries SET metadata_json = '[]' WHERE id = ?")
                .bind(id.raw())
                .execute(&storage.pool)
                .await
                .is_err()
        );
    }

    use super::*;

    #[tokio::test]
    async fn file_metadata_reads_existing_optional_mime_without_inference() {
        let dir = tempfile::tempdir().unwrap();
        let db =
            SqliteStorage::connect(format!("sqlite://{}", dir.path().join("mime.db").display()))
                .await
                .unwrap();
        let id = db.create_file("/payload.json", "{}").await.unwrap();
        assert_eq!(db.get_file_meta(id).await.unwrap().mime_type(), None);
        sqlx::query("UPDATE entries SET mime_type=? WHERE id=?")
            .bind("text/x-shellscript")
            .bind(id.raw())
            .execute(&db.pool)
            .await
            .unwrap();
        assert_eq!(
            db.get_file_meta(id).await.unwrap().mime_type(),
            Some("text/x-shellscript")
        );
    }

    #[test]
    fn cte_path_segments_json_roundtrip() {
        let s = vec!["a".into(), "b".into(), "42".into()];
        let j = path_segments_json(&s).unwrap();
        assert_eq!(j, r#"["a","b","42"]"#);
    }

    #[tokio::test]
    async fn cte_resolve_path_smoke() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("cte.db");
        let uri = format!("sqlite://{}", db_path.display());
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(
                uri.parse::<SqliteConnectOptions>()
                    .unwrap()
                    .create_if_missing(true)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        migrate(&pool).await.unwrap();
        super::snapshots::migrate(&pool).await.unwrap();
        let now = Timestamp::now();
        let id_a: i64 = sqlx::query_scalar(
            r"INSERT INTO entries (parent_id, kind, name, description, content, size, created_at, modified_at, accessed_at)
            VALUES (1, 0, 'alpha', NULL, NULL, NULL, ?, ?, ?) RETURNING id",
        )
        .bind(now)
        .bind(now)
        .bind(now)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            r"INSERT INTO entries (parent_id, kind, name, description, content, size, created_at, modified_at, accessed_at)
            VALUES (?, 0, 'beta', NULL, NULL, NULL, ?, ?, ?)",
        )
        .bind(id_a)
        .bind(now)
        .bind(now)
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();
        let id = resolve_path_id(
            &pool,
            &["alpha".into(), "beta".into()],
            Some(EntryKind::Dir),
        )
        .await
        .unwrap();
        let path = canonical_path_for_id(&pool, id).await.unwrap();
        assert_eq!(path, "/alpha/beta");
        let root = resolve_path_id(&pool, &[], Some(EntryKind::Dir))
            .await
            .unwrap();
        assert_eq!(root.raw(), ROOT_ID);
    }
}

#[cfg(test)]
mod flag_tests;
