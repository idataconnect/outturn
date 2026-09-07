pub mod memory;
pub mod scope;
pub mod s3;

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
    async fn read(
        &self,
        path: &str,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>, StorageError>;

    async fn write(
        &self,
        path: &str,
        offset: u64,
        data: &[u8],
    ) -> Result<u64, StorageError>;

    async fn stat(&self, path: &str) -> Result<FileMetadata, StorageError>;

    async fn list(&self, prefix: &str) -> Result<Vec<FileMetadata>, StorageError>;

    async fn delete(&self, path: &str) -> Result<(), StorageError>;
}

/// Why a storage call did not do what was asked.
///
/// The variants separate two owners. `NotFound` and `PermissionDenied` are
/// outcomes of what an agent asked for, and belong to the tenant: they are
/// reported on the transcript and nowhere else. `Unavailable` means the
/// platform is broken -- no bucket, no connection, rejected credentials --
/// and is the only variant an operator should ever be told about.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("not found")]
    NotFound,
    #[error("permission denied")]
    PermissionDenied,
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
