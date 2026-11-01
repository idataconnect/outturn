//! The S3 backend against a real bucket.
//!
//! Multipart is where an S3 client goes wrong quietly -- a part numbered from
//! zero, a last part sent twice, an upload never aborted -- and the in-memory
//! backend cannot catch any of it. Skaffold forwards the local MinIO:
//!
//!   TEST_S3_URL=http://localhost:19000 cargo test --features integration-tests --test storage_s3

use outturn::storage::s3::PART_BYTES;
use outturn::storage::{S3Storage, StorageBackend, StorageError};

async fn store() -> S3Storage {
    let url = std::env::var("TEST_S3_URL")
        .expect("TEST_S3_URL must be set, e.g. http://localhost:19000 (forwarded by skaffold)");
    // A prefix of its own per run, so runs neither collide nor clean up.
    let prefix = format!("test/{}", uuid::Uuid::now_v7());
    let s = S3Storage::new(&url, "outturn", "outturn", "outturn-dev", prefix).expect("store");
    s.ensure_bucket().await.expect("bucket");
    s
}

/// Not a repeating pattern, so a part sent twice or out of order shows.
fn bytes(n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
        .collect()
}

async fn read_all(s: &S3Storage, path: &str) -> Vec<u8> {
    let mut r = s.open_reader(path).await.expect("open reader");
    let mut out = Vec::new();
    loop {
        let piece = r.read(1 << 20).await.expect("read");
        if piece.is_empty() {
            return out;
        }
        out.extend(piece);
    }
}

#[tokio::test]
async fn a_small_object_goes_up_whole() {
    let s = store().await;
    let mut w = s.open_writer("small.bin").await.unwrap();
    w.write(b"hello ").await.unwrap();
    w.write(b"world").await.unwrap();
    assert_eq!(w.finish().await.unwrap(), 11);
    assert_eq!(read_all(&s, "small.bin").await, b"hello world");
}

#[tokio::test]
async fn a_large_object_goes_up_in_parts_and_comes_back_intact() {
    let s = store().await;
    // Two full parts and a short last one, written in chunks that do not
    // line up with part boundaries.
    let data = bytes(PART_BYTES * 2 + 12_345);
    let mut w = s.open_writer("large.bin").await.unwrap();
    for chunk in data.chunks(700_001) {
        w.write(chunk).await.unwrap();
    }
    assert_eq!(w.finish().await.unwrap(), data.len() as u64);
    assert_eq!(s.stat("large.bin").await.unwrap().size, data.len() as u64);
    assert!(read_all(&s, "large.bin").await == data, "contents differ");
}

#[tokio::test]
async fn a_writer_dropped_partway_leaves_the_old_object() {
    let s = store().await;
    s.write("kept.bin", b"before").await.unwrap();
    let mut w = s.open_writer("kept.bin").await.unwrap();
    // Past one part, so an upload has started and has something to abort.
    w.write(&bytes(PART_BYTES + 1)).await.unwrap();
    drop(w);
    assert_eq!(read_all(&s, "kept.bin").await, b"before");
}

#[tokio::test]
async fn a_reader_on_nothing_is_not_found() {
    let s = store().await;
    assert!(matches!(
        s.open_reader("absent.bin").await.err(),
        Some(StorageError::NotFound)
    ));
}

#[tokio::test]
async fn the_lifecycle_rules_are_accepted() {
    let s = store().await;
    s.ensure_lifecycle().await.expect("lifecycle rules refused");
    // Twice: the second finds the first's document, or none, and is the
    // same answer either way.
    s.ensure_lifecycle()
        .await
        .expect("lifecycle rules refused again");
}

/// An upload a person sends in pieces: pieces out of order and one sent
/// twice still join into the file, and nothing is at the path until then.
#[tokio::test]
async fn an_upload_sent_in_pieces_joins_in_order_and_replaces_only_when_whole() {
    let s = store().await;
    s.write("upload.bin", b"the old file").await.unwrap();
    let piece = 5 * 1024 * 1024;
    let data = bytes(piece * 2 + 777);
    let id = s.start_upload("upload.bin").await.unwrap();
    let three = s
        .put_upload_part("upload.bin", &id, 3, data[piece * 2..].to_vec())
        .await
        .unwrap();
    let one = s
        .put_upload_part("upload.bin", &id, 1, data[..piece].to_vec())
        .await
        .unwrap();
    s.put_upload_part("upload.bin", &id, 2, vec![0; piece])
        .await
        .unwrap();
    let two = s
        .put_upload_part("upload.bin", &id, 2, data[piece..piece * 2].to_vec())
        .await
        .unwrap();
    assert_eq!(read_all(&s, "upload.bin").await, b"the old file");
    s.complete_upload("upload.bin", &id, &[(1, one), (2, two), (3, three)])
        .await
        .unwrap();
    assert!(read_all(&s, "upload.bin").await == data);
}

#[tokio::test]
async fn an_abandoned_upload_leaves_nothing_behind() {
    let s = store().await;
    let id = s.start_upload("gone.bin").await.unwrap();
    s.put_upload_part("gone.bin", &id, 1, vec![1; 1000])
        .await
        .unwrap();
    s.abort_upload("gone.bin", &id).await.unwrap();
    assert!(matches!(
        s.stat("gone.bin").await,
        Err(StorageError::NotFound)
    ));
}
