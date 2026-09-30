// A downstream exhaustive match over the published 2.0 error variants must
// continue to compile. Budget failures belong to the new UpdateError API.
fn legacy_error_match(error: ic_sqlite_vfs::StableMemoryError) -> &'static str {
    use ic_sqlite_vfs::StableMemoryError;
    match error {
        StableMemoryError::NotInitialized => "NotInitialized",
        StableMemoryError::AlreadyInitialized => "AlreadyInitialized",
        StableMemoryError::MemoryAlreadyRegistered => "MemoryAlreadyRegistered",
        StableMemoryError::GrowFailed { .. } => "GrowFailed",
        StableMemoryError::ReadOutOfBounds { .. } => "ReadOutOfBounds",
        StableMemoryError::OffsetOverflow => "OffsetOverflow",
        StableMemoryError::ImportAlreadyStarted => "ImportAlreadyStarted",
        StableMemoryError::ImportNotStarted => "ImportNotStarted",
        StableMemoryError::UpdateInProgress => "UpdateInProgress",
        StableMemoryError::ImportOutOfOrder { .. } => "ImportOutOfOrder",
        StableMemoryError::ImportOutOfBounds { .. } => "ImportOutOfBounds",
        StableMemoryError::ImportIncomplete { .. } => "ImportIncomplete",
        StableMemoryError::ChecksumMismatch { .. } => "ChecksumMismatch",
        StableMemoryError::ChecksumRefreshChunkEmpty => "ChecksumRefreshChunkEmpty",
        StableMemoryError::Failpoint(..) => "Failpoint",
        StableMemoryError::MetaChecksumMismatch => "MetaChecksumMismatch",
        StableMemoryError::UnsupportedLayoutVersion(..) => "UnsupportedLayoutVersion",
        StableMemoryError::ForeignStableMemoryImage => "ForeignStableMemoryImage",
        StableMemoryError::ZeroExtentLimitExceeded { .. } => "ZeroExtentLimitExceeded",
    }
}

#[test]
fn published_error_variants_remain_exhaustively_matchable() {
    assert_eq!(
        legacy_error_match(ic_sqlite_vfs::StableMemoryError::NotInitialized),
        "NotInitialized"
    );
    assert_eq!(
        legacy_error_match(ic_sqlite_vfs::StableMemoryError::Failpoint("probe")),
        "Failpoint"
    );
}
