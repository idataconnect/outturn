pub mod memory;
pub mod s3;
pub mod scope;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetadata {
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
}

#[async_trait]
pub trait StorageBackend: Send + Sync {
    async fn read(&self, path: &str, offset: u64, len: u32) -> Result<Vec<u8>, StorageError>;

    /// Writes a whole object, replacing whatever was there.
    async fn write(&self, path: &str, data: &[u8]) -> Result<u64, StorageError>;

    /// Opens an object to be written in pieces. Nothing appears at the path
    /// until the writer finishes, and a writer dropped unfinished leaves
    /// whatever was there before (see docs/streaming-storage.md).
    async fn open_writer(&self, path: &str) -> Result<Box<dyn ObjectWriter>, StorageError>;

    /// Opens an object to be read from the front, in pieces, over one request.
    async fn open_reader(&self, path: &str) -> Result<Box<dyn ObjectReader>, StorageError>;

    async fn stat(&self, path: &str) -> Result<FileMetadata, StorageError>;

    async fn list(&self, prefix: &str) -> Result<Vec<FileMetadata>, StorageError>;

    /// At most `limit` entries under `prefix`, in key order, starting after the
    /// key `after` when one is given -- and whether there are more.
    ///
    /// For a listing somebody is waiting on. `list` reads everything under a
    /// prefix, which is right for a guest walking its own files and wrong for a
    /// page showing a workspace scope that only ever grows. The default reads
    /// everything and cuts it; a backend that can ask for one page does.
    async fn list_page(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<FileMetadata>, bool), StorageError> {
        let mut all = self.list(prefix).await?;
        all.sort_by(|a, b| a.path.cmp(&b.path));
        let mut page: Vec<FileMetadata> = all
            .into_iter()
            .filter(|f| after.is_none_or(|a| f.path.as_str() > a))
            .take(limit + 1)
            .collect();
        let more = page.len() > limit;
        page.truncate(limit);
        Ok((page, more))
    }

    async fn delete(&self, path: &str) -> Result<(), StorageError>;
}

/// An object being written. See `StorageBackend::open_writer`.
#[async_trait]
pub trait ObjectWriter: Send {
    /// Appends.
    async fn write(&mut self, chunk: &[u8]) -> Result<(), StorageError>;

    /// Completes the object and says how large it is. Only after this does
    /// anything exist at the path.
    async fn finish(self: Box<Self>) -> Result<u64, StorageError>;
}

/// An object being read. See `StorageBackend::open_reader`.
#[async_trait]
pub trait ObjectReader: Send {
    /// Up to `max` bytes; empty once the object is exhausted.
    async fn read(&mut self, max: usize) -> Result<Vec<u8>, StorageError>;
}

/// Why a storage call did not do what was asked.
///
/// The variants separate two owners. `NotFound` and `PermissionDenied` are
/// outcomes of what an agent asked for, and belong to the workspace: they are
/// reported on the transcript and nowhere else. `Unavailable` means the
/// platform is broken -- no bucket, no connection, rejected credentials --
/// and is the only variant an operator should ever be told about.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("not found")]
    NotFound,
    #[error("permission denied")]
    PermissionDenied,
    /// Refused for a reason the caller can act on, said in full.
    #[error("{0}")]
    Refused(String),
    /// The store itself cannot be used. Nobody downstream of the operator
    /// caused this, and nobody downstream can fix it.
    #[error("object storage is unavailable: {0}")]
    Unavailable(String),
    #[error("io error: {0}")]
    Io(String),
}

impl StorageError {
    /// Whether this is the platform's fault rather than the caller's.
    pub fn is_platform_fault(&self) -> bool {
        matches!(self, StorageError::Unavailable(_))
    }
}

pub use memory::MemoryStorage;
pub use s3::S3Storage;
