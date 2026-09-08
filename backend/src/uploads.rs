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
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::error::AppError;

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

    let temp_name = format!(".tmp-{}", uuid::Uuid::new_v4());
    let temp_path = format!("{}/{}", files_dir, temp_name);

    let mut file = tokio::fs::File::create(&temp_path)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let mut hasher = Sha256::new();
    let mut size: u64 = 0;

    loop {
        let chunk = match field.chunk().await {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(e) => {
                cleanup_temp(&temp_path).await;
                return Err(AppError::BadRequest(e.to_string()));
            }
        };

        size += chunk.len() as u64;
        if size > max_bytes {
            cleanup_temp(&temp_path).await;
            return Err(AppError::BadRequest(format!(
                "File too large (max {:.1} GB)",
                max_bytes as f64 / 1_073_741_824.0
            )));
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
        sha256_hex: format!("{:x}", hasher.finalize()),
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
