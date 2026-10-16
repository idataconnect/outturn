use async_trait::async_trait;

use super::{FileMetadata, ObjectReader, ObjectWriter, StorageBackend, StorageError};

type Files = std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, Vec<u8>>>>;

pub struct MemoryStorage {
    files: Files,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self {
            files: Default::default(),
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
    async fn read(&self, path: &str, offset: u64, len: u32) -> Result<Vec<u8>, StorageError> {
        let files = self.files.read().await;
        let data = files.get(path).ok_or(StorageError::NotFound)?;
        let start = offset as usize;
        if start >= data.len() {
            return Ok(vec![]);
        }
        let end = (start + len as usize).min(data.len());
        Ok(data[start..end].to_vec())
    }

    async fn write(&self, path: &str, data: &[u8]) -> Result<u64, StorageError> {
        let mut files = self.files.write().await;
        files.insert(path.to_string(), data.to_vec());
        Ok(data.len() as u64)
    }

    async fn open_writer(&self, path: &str) -> Result<Box<dyn ObjectWriter>, StorageError> {
        Ok(Box::new(MemoryWriter {
            files: self.files.clone(),
            path: path.to_string(),
            buf: Vec::new(),
        }))
    }

    async fn open_reader(&self, path: &str) -> Result<Box<dyn ObjectReader>, StorageError> {
        let files = self.files.read().await;
        let data = files.get(path).ok_or(StorageError::NotFound)?.clone();
        Ok(Box::new(MemoryReader { data, at: 0 }))
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

/// Held aside until finished, so a writer dropped partway leaves the old
/// object in place -- the property the S3 writer gets from multipart.
struct MemoryWriter {
    files: Files,
    path: String,
    buf: Vec<u8>,
}

#[async_trait]
impl ObjectWriter for MemoryWriter {
    async fn write(&mut self, chunk: &[u8]) -> Result<(), StorageError> {
        self.buf.extend_from_slice(chunk);
        Ok(())
    }

    async fn finish(self: Box<Self>) -> Result<u64, StorageError> {
        let size = self.buf.len() as u64;
        self.files.write().await.insert(self.path, self.buf);
        Ok(size)
    }
}

struct MemoryReader {
    data: Vec<u8>,
    at: usize,
}

#[async_trait]
impl ObjectReader for MemoryReader {
    async fn read(&mut self, max: usize) -> Result<Vec<u8>, StorageError> {
        let end = (self.at + max).min(self.data.len());
        let out = self.data[self.at..end].to_vec();
        self.at = end;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_writer_dropped_unfinished_leaves_the_old_object() {
        let store = MemoryStorage::new();
        store.write("a", b"before").await.unwrap();
        let mut w = store.open_writer("a").await.unwrap();
        w.write(b"half of something").await.unwrap();
        drop(w);
        assert_eq!(store.read_file("a").await.unwrap(), b"before");
    }

    #[tokio::test]
    async fn a_reader_returns_the_object_in_pieces_then_nothing() {
        let store = MemoryStorage::new();
        store.write("a", b"abcdefg").await.unwrap();
        let mut w = store.open_writer("b").await.unwrap();
        w.write(b"abc").await.unwrap();
        w.write(b"defg").await.unwrap();
        assert_eq!(w.finish().await.unwrap(), 7);

        let mut r = store.open_reader("b").await.unwrap();
        let mut got = Vec::new();
        loop {
            let piece = r.read(3).await.unwrap();
            if piece.is_empty() {
                break;
            }
            assert!(piece.len() <= 3);
            got.extend(piece);
        }
        assert_eq!(got, b"abcdefg");
    }
}
