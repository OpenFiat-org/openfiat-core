//! The consuming half of OFS-1300 §13-17: download a snapshot from one
//! of its announced locations, then hand the bytes to
//! [`SnapshotIndex::import`], which verifies before it trusts.
//!
//! Nothing here decides whether a snapshot is *good* — that judgement
//! lives entirely in `import`, against the signed metadata this node
//! already holds. This module's only jobs are to move bytes and to refuse
//! to move more of them than the announcement said existed.

use crate::error::SnapshotError;
use crate::location::SnapshotLocation;
use crate::record::SnapshotId;
use crate::store::SnapshotIndex;
use openfiat_storage::KvStore;
use std::time::Duration;

/// A download that has not finished inside this is a stall, not a slow
/// link: the whole transfer is bounded, not each read, so a peer that
/// trickles a byte at a time cannot hold a bootstrapping node open
/// indefinitely.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// Downloads `location` into memory, refusing to accept more than
/// `size_bytes`.
///
/// The cap is enforced twice — against the advertised `Content-Length`
/// before the body is read, and against the running total as chunks
/// arrive, because `Content-Length` is a claim by the same host serving
/// the body. Without the second check a hostile mirror answers a
/// 4 KiB snapshot request with an endless body and a joining node
/// exhausts its memory before it ever reaches a hash comparison.
pub async fn download(
    client: &reqwest::Client,
    location: &SnapshotLocation,
    size_bytes: u64,
) -> Result<Vec<u8>, SnapshotError> {
    // Checked before a request is made, not after the body arrives: the
    // point of the cap is that this node never allocates that much, and
    // `size_bytes` is known from the announcement it already verified.
    if size_bytes > crate::codec::MAX_SNAPSHOT_BYTES {
        return Err(SnapshotError::SnapshotTooLarge);
    }
    let mut response = client
        .get(location.as_str())
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .map_err(|_| SnapshotError::DownloadFailed)?;
    if !response.status().is_success() {
        return Err(SnapshotError::DownloadFailed);
    }
    if response
        .content_length()
        .is_some_and(|len| len > size_bytes)
    {
        return Err(SnapshotError::SizeMismatch);
    }

    let mut bytes = Vec::with_capacity(size_bytes.min(1 << 20) as usize);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| SnapshotError::DownloadFailed)?
    {
        if bytes.len() as u64 + chunk.len() as u64 > size_bytes {
            return Err(SnapshotError::SizeMismatch);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Fetches and imports the snapshot `id`, returning how many state
/// entries were restored.
///
/// `id` names an announcement this node already holds — the metadata,
/// including every location tried here, comes from
/// [`SnapshotIndex::get`] and therefore from a signature this node
/// verified and a producer the service registry vouched for. A snapshot
/// nobody announced cannot be fetched through this function at all, which
/// is the point: a caller cannot talk this node into importing state from
/// a URL of the caller's choosing.
///
/// Locations are tried in announced order, and a location that fails
/// verification is treated exactly like one that failed to connect — the
/// next mirror gets a turn. That is safe precisely because the state root
/// is what decides, so a bad mirror costs a retry and can never cost
/// correctness. If every location fails, the last failure is returned
/// rather than a generic one, so an operator sees whether they are
/// looking at a network problem or a corrupt snapshot.
pub async fn fetch_and_import<S: KvStore>(
    index: &SnapshotIndex<S>,
    client: &reqwest::Client,
    id: &SnapshotId,
) -> Result<usize, SnapshotError> {
    let metadata = index.get(id).ok_or(SnapshotError::UnknownSnapshot)?;
    if metadata.locations.is_empty() {
        return Err(SnapshotError::NoLocationAdvertised);
    }

    let mut last_error = SnapshotError::DownloadFailed;
    for location in &metadata.locations {
        match download(client, location, metadata.size_bytes).await {
            Ok(bytes) => match index.import(id, &bytes) {
                Ok(restored) => return Ok(restored),
                Err(error) => last_error = error,
            },
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}
