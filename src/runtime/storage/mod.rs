pub mod memory;
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

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("not found")]
    NotFound,
    #[error("permission denied")]
    PermissionDenied,
    #[error("io error: {0}")]
    Io(String),
}

pub use memory::MemoryStorage;
pub use s3::S3Storage;
