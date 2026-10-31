// Chunked multipart upload helper. Streams a single multipart field's
// body to a temp file under the configured `files/` directory, hashing
// (Sha256) as it goes and enforcing a max-size cap. Keeps per-upload
// memory bounded — the slurp-into-Vec path read whole files into RAM,
// which OOMs the backend at the 1 GB ceiling.
//
// The temp file is created inside the SAME directory as final blobs so
// the eventual rename is atomic (same filesystem). On any error path
// (oversize, write failure, multipart error) the temp file is deleted
// before returning, so callers don't have to clean up on the error
// branch.

use axum::extract::multipart::Field;
use axum::extract::Multipart;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::error::{multipart_error, AppError};

/// Space every upload leaves free on the data volume. The SQLite database, its
/// WAL, the logs and OME all write to the same disk, and a full one takes the
/// whole service down with it — so an upload stops here rather than at zero.
pub const MIN_FREE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// How often (in bytes written) a streaming upload re-checks the free space.
const FREE_SPACE_CHECK_EVERY: u64 = 64 * 1024 * 1024;

/// Bytes available to an unprivileged writer on the volume holding `dir`.
/// `None` when it can't be read, which is treated as "no constraint" — the
/// floor is a safety margin, not a reason to refuse every upload on a platform
/// where `statvfs` fails.
fn available_bytes(dir: &str) -> Option<u64> {
    let st = rustix::fs::statvfs(dir).ok()?;
    Some(st.f_bavail.saturating_mul(st.f_frsize))
}

fn out_of_space(dir: &str) -> bool {
    available_bytes(dir).is_some_and(|free| free < MIN_FREE_BYTES)
}

fn storage_full() -> AppError {
    AppError::InsufficientStorage("The server is out of storage space".into())
}

/// How much of a refused upload is read and thrown away before the refusal is
/// sent (see [`drain`]).
pub const DRAIN_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Read and discard up to `budget` more bytes of `field`. Returns what is left
/// of the budget, or `None` once it is spent — at which point the caller must
/// stop: asking the multipart stream for another field makes it skim the rest
/// of this one, unbounded.
async fn discard(field: &mut Field<'_>, mut budget: u64) -> Option<u64> {
    while let Ok(Some(chunk)) = field.chunk().await {
        budget = budget.checked_sub(chunk.len() as u64)?;
    }
    Some(budget)
}

/// [`discard`] for a field whose upload is being refused part-way.
pub async fn discard_rest(field: &mut Field<'_>) {
    let _ = discard(field, DRAIN_MAX_BYTES).await;
}

/// Read and discard what is left of a multipart body — up to
/// [`DRAIN_MAX_BYTES`] — so that a refusal sent before the upload has been read
/// actually reaches the client.
///
/// When a handler answers without reading the request body, hyper closes the
/// connection, and Caddy, still writing the upload upstream, fails with a broken
/// pipe and answers 502 with an empty body instead of the refusal — measured:
/// one in five 1 MB uploads to an ended room. Reading the rest first keeps the
/// connection whole. Only for callers already authenticated: draining for anyone
/// would let an anonymous client make the server ingest data for nothing. A body
/// larger than the bound still races, which is why the clients check sizes
/// before sending.
pub async fn drain(multipart: &mut Multipart) {
    let mut budget = DRAIN_MAX_BYTES;
    while let Ok(Some(mut field)) = multipart.next_field().await {
        match discard(&mut field, budget).await {
            Some(left) => budget = left,
            None => return,
        }
    }
}

pub struct StoredUpload {
    /// Filename of the temp blob, sitting in `files_dir`. Callers must
    /// either rename it to its final name on success or remove it on
    /// dedup hit. Leaving it dangling would leak disk space.
    pub temp_name: String,
    pub size: u64,
    pub sha256_hex: String,
}

/// Stream `field` to `{files_dir}/.tmp-<uuid>`, returning size + hash.
/// On any error the temp file is deleted before this returns.
pub async fn stream_field_to_temp(
    field: &mut Field<'_>,
    files_dir: &str,
    max_bytes: u64,
) -> Result<StoredUpload, AppError> {
    tokio::fs::create_dir_all(files_dir)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if out_of_space(files_dir) {
        return Err(storage_full());
    }

    let temp_name = format!(".tmp-{}", uuid::Uuid::new_v4());
    let temp_path = format!("{}/{}", files_dir, temp_name);

    let mut file = tokio::fs::File::create(&temp_path)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let mut hasher = Sha256::new();
    let mut size: u64 = 0;
    let mut next_space_check = FREE_SPACE_CHECK_EVERY;

    loop {
        let chunk = match field.chunk().await {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(e) => {
                cleanup_temp(&temp_path).await;
                return Err(multipart_error(e));
            }
        };

        size += chunk.len() as u64;
        if size > max_bytes {
            cleanup_temp(&temp_path).await;
            discard_rest(field).await;
            return Err(AppError::PayloadTooLarge(format!(
                "File too large (max {:.1} GB)",
                max_bytes as f64 / 1_073_741_824.0
            )));
        }
        // Checked as the file grows, not just once up front: several uploads
        // can be in flight at once, and none of them knows its size until the
        // last byte arrives.
        if size >= next_space_check {
            next_space_check = size + FREE_SPACE_CHECK_EVERY;
            if out_of_space(files_dir) {
                cleanup_temp(&temp_path).await;
                discard_rest(field).await;
                return Err(storage_full());
            }
        }

        hasher.update(&chunk);
        if let Err(e) = file.write_all(&chunk).await {
            cleanup_temp(&temp_path).await;
            return Err(AppError::Internal(e.to_string()));
        }
    }

    if let Err(e) = file.flush().await {
        cleanup_temp(&temp_path).await;
        return Err(AppError::Internal(e.to_string()));
    }
    drop(file);

    Ok(StoredUpload {
        temp_name,
        size,
        sha256_hex: hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    })
}

async fn cleanup_temp(path: &str) {
    let _ = tokio::fs::remove_file(path).await;
}

/// Remove `.tmp-*` files in `files_dir` older than `max_age`. Called at
/// startup to clean up after a crash mid-upload. Best-effort: any error
/// is logged and otherwise ignored.
pub async fn sweep_stale_temps(files_dir: &str, max_age: std::time::Duration) {
    let mut dir = match tokio::fs::read_dir(files_dir).await {
        Ok(d) => d,
        Err(_) => return, // dir may not exist yet on a fresh deploy
    };
    let now = std::time::SystemTime::now();
    let mut removed = 0u32;
    while let Ok(Some(entry)) = dir.next_entry().await {
        let name = entry.file_name();
        let name_str = match name.to_str() {
            Some(s) => s,
            None => continue,
        };
        if !name_str.starts_with(".tmp-") {
            continue;
        }
        let meta = match entry.metadata().await {
            Ok(m) => m,
            Err(_) => continue,
        };
        let modified = match meta.modified() {
            Ok(t) => t,
            Err(_) => continue,
        };
        let is_stale = now
            .duration_since(modified)
            .map(|age| age > max_age)
            .unwrap_or(false);
        if is_stale && tokio::fs::remove_file(entry.path()).await.is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::info!(removed, "swept stale upload temp files");
    }
}
