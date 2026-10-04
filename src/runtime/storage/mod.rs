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

    async fn write(&self, path: &str, offset: u64, data: &[u8]) -> Result<u64, StorageError>;

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
