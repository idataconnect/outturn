use async_trait::async_trait;

use super::{FileMetadata, StorageBackend, StorageError};

pub struct MemoryStorage {
    files: tokio::sync::RwLock<std::collections::HashMap<String, Vec<u8>>>,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self {
            files: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        }
    }

    pub async fn write_file(&self, path: &str, data: &[u8]) {
        let mut files = self.files.write().await;
        files.insert(path.to_string(), data.to_vec());
    }

    pub async fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        let files = self.files.read().await;
        files.get(path).cloned()
    }
}

impl Default for MemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl StorageBackend for MemoryStorage {
    async fn read(
        &self,
        path: &str,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>, StorageError> {
        let files = self.files.read().await;
        let data = files.get(path).ok_or(StorageError::NotFound)?;
        let start = offset as usize;
        if start >= data.len() {
            return Ok(vec![]);
        }
        let end = (start + len as usize).min(data.len());
        Ok(data[start..end].to_vec())
    }

    async fn write(
        &self,
        path: &str,
        offset: u64,
        data: &[u8],
    ) -> Result<u64, StorageError> {
        let mut files = self.files.write().await;
        let file = files.entry(path.to_string()).or_default();
        let start = offset as usize;
        if start + data.len() > file.len() {
            file.resize(start + data.len(), 0);
        }
        file[start..start + data.len()].copy_from_slice(data);
        Ok(data.len() as u64)
    }

    async fn stat(&self, path: &str) -> Result<FileMetadata, StorageError> {
        let files = self.files.read().await;
        let data = files.get(path).ok_or(StorageError::NotFound)?;
        Ok(FileMetadata {
            path: path.to_string(),
            size: data.len() as u64,
            is_dir: false,
        })
    }

    async fn list(&self, prefix: &str) -> Result<Vec<FileMetadata>, StorageError> {
        let files = self.files.read().await;
        let entries = files
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| FileMetadata {
                path: k.clone(),
                size: v.len() as u64,
                is_dir: false,
            })
            .collect();
        Ok(entries)
    }

    async fn delete(&self, path: &str) -> Result<(), StorageError> {
        let mut files = self.files.write().await;
        files.remove(path).ok_or(StorageError::NotFound)?;
        Ok(())
    }
}
