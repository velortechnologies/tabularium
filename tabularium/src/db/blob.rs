//! Private immutable blob generations. Entry rows select the live generation.
use super::EntryId;
use crate::{Error, Result};
use std::path::{Path, PathBuf};

pub(crate) struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub(crate) fn temporary(&self) -> Result<tempfile::NamedTempFile> {
        std::fs::create_dir_all(&self.root)?;
        Ok(tempfile::NamedTempFile::new_in(&self.root)?)
    }
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(crate) fn path(&self, id: EntryId, generation: &str) -> Result<PathBuf> {
        if id.raw() <= 0 || uuid::Uuid::parse_str(generation).is_err() {
            return Err(Error::InvalidInput("invalid private blob identity".into()));
        }
        Ok(self
            .root
            .join(format!("{:03}", id.raw() / 1_000_000))
            .join(format!("{:03}", id.raw() / 1000 % 1000))
            .join(format!("{}.{}", id.raw(), generation)))
    }

    /// Publish a fully written same-filesystem temporary file before committing its row.
    /// No replace-in-place: a generation must be unique, and existing readers retain their inode.
    pub(crate) fn publish(
        &self,
        temporary: &Path,
        id: EntryId,
        generation: &str,
    ) -> Result<PathBuf> {
        let path = self.path(id, generation)?;
        let parent = path
            .parent()
            .ok_or_else(|| Error::InvalidInput("missing blob parent".into()))?;
        std::fs::create_dir_all(parent)?;
        // Persist every newly created directory link, not only the leaf.
        let mut directory = Some(parent);
        while let Some(current) = directory {
            std::fs::File::open(current)?.sync_all()?;
            if current == self.root {
                break;
            }
            directory = current.parent();
        }
        if let Some(parent) = self.root.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        std::fs::File::open(temporary)?.sync_all()?;
        // Atomic exclusive publication: unlike rename, hard_link cannot overwrite a generation.
        std::fs::hard_link(temporary, &path)?;
        std::fs::File::open(parent)?.sync_all()?;
        std::fs::remove_file(temporary)?;
        Ok(path)
    }

    pub(crate) fn remove(&self, id: EntryId, generation: &str) -> Result<()> {
        let path = self.path(id, generation)?;
        match std::fs::remove_file(&path) {
            Ok(()) => {
                std::fs::File::open(path.parent().unwrap())?.sync_all()?;
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn publish_is_exclusive_and_unlink_preserves_an_open_reader() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path().join("blobs"));
        let generation = uuid::Uuid::new_v4().to_string();
        let temp = dir.path().join("upload");
        std::fs::write(&temp, [0, 255, 17]).unwrap();
        let path = store.publish(&temp, 1_234_567.into(), &generation).unwrap();
        assert!(path.ends_with(format!("001/234/1234567.{generation}")));
        let mut reader = std::fs::File::open(&path).unwrap();
        std::fs::write(&temp, b"replacement").unwrap();
        assert!(store.publish(&temp, 1_234_567.into(), &generation).is_err());
        store.remove(1_234_567.into(), &generation).unwrap();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, [0, 255, 17]);
        assert!(!path.exists());
        assert!(store.path(123.into(), "../../outside").is_err());
    }
}
