// Copyright 2020 Tyler Neely
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//

//! Rust wrapper for RocksDB.
//!
//! # Examples
//!
//! ```
//! use rocksdb::{DB, Options};
//! // NB: db is automatically closed at end of lifetime
//! let path = "_path_for_rocksdb_storage";
//! {
//!    let db = DB::open_default(path).unwrap();
//!    db.put(b"my key", b"my value").unwrap();
//!    match db.get(b"my key") {
//!        Ok(Some(value)) => println!("retrieved value {}", String::from_utf8(value).unwrap()),
//!        Ok(None) => println!("value not found"),
//!        Err(e) => println!("operational problem encountered: {}", e),
//!    }
//!    db.delete(b"my key").unwrap();
//! }
//! let _ = DB::destroy(&Options::default(), path);
//! ```
//!
//! Opening a database and a single column family with custom options:
//!
//! ```
//! use rocksdb::{DB, ColumnFamilyDescriptor, Options};
//!
//! let path = "_path_for_rocksdb_storage_with_cfs";
//! let mut cf_opts = Options::default();
//! cf_opts.set_max_write_buffer_number(16);
//! let cf = ColumnFamilyDescriptor::new("cf1", cf_opts);
//!
//! let mut db_opts = Options::default();
//! db_opts.create_missing_column_families(true);
//! db_opts.create_if_missing(true);
//! {
//!     let db = DB::open_cf_descriptors(&db_opts, path, vec![cf]).unwrap();
//! }
//! let _ = DB::destroy(&db_opts, path);
//! ```
//!

#![warn(clippy::pedantic)]
#![allow(
    // Next `cast_*` lints don't give alternatives.
    clippy::cast_possible_wrap, clippy::cast_possible_truncation, clippy::cast_sign_loss,
    // Next lints produce too much noise/false positives.
    clippy::module_name_repetitions, clippy::similar_names, clippy::must_use_candidate,
    // '... may panic' lints.
    // Too much work to fix.
    clippy::missing_errors_doc,
    // False positive: WebSocket
    clippy::doc_markdown,
    clippy::missing_safety_doc,
    clippy::needless_pass_by_value,
    clippy::ptr_as_ptr,
    clippy::missing_panics_doc,
    clippy::from_over_into,
)]

#[macro_use]
mod ffi_util;

pub mod backup;
pub mod checkpoint;
mod column_family;
pub mod compaction_filter;
pub mod compaction_filter_factory;
mod comparator;
mod db;
mod db_iterator;
mod db_options;
mod db_pinnable_slice;
mod env;
mod iter_range;
pub mod merge_operator;
pub mod perf;
mod prop_name;
pub mod properties;
mod slice_transform;
mod snapshot;
mod sst_file_writer;
mod transactions;
mod write_batch;

pub use crate::{
    column_family::{
        AsColumnFamilyRef, BoundColumnFamily, ColumnFamily, ColumnFamilyDescriptor,
        ColumnFamilyRef, DEFAULT_COLUMN_FAMILY_NAME,
    },
    compaction_filter::Decision as CompactionDecision,
    db::{
        DBAccess, DBCommon, DBWithThreadMode, LiveFile, MultiThreaded, SingleThreaded, ThreadMode,
        DB,
    },
    db_iterator::{
        DBIterator, DBIteratorWithThreadMode, DBRawIterator, DBRawIteratorWithThreadMode,
        DBWALIterator, Direction, IteratorMode,
    },
    db_options::{
        BlockBasedIndexType, BlockBasedOptions, BottommostLevelCompaction, Cache, ChecksumType,
        CompactOptions, CuckooTableOptions, DBCompactionStyle, DBCompressionType, DBPath,
        DBRecoveryMode, DataBlockIndexType, FifoCompactOptions, FlushOptions,
        IngestExternalFileOptions, KeyEncodingType, LogLevel, MemtableFactory, Options,
        PlainTableFactoryOptions, ReadOptions, ReadTier, UniversalCompactOptions,
        UniversalCompactionStopStyle, WaitForCompactOptions, WriteOptions,
    },
    db_pinnable_slice::DBPinnableSlice,
    env::Env,
    ffi_util::CStrLike,
    iter_range::{IterateBounds, PrefixRange},
    merge_operator::MergeOperands,
    perf::{PerfContext, PerfMetric, PerfStatsLevel},
    slice_transform::SliceTransform,
    snapshot::{Snapshot, SnapshotWithThreadMode},
    sst_file_writer::SstFileWriter,
    transactions::{
        OptimisticTransactionDB, OptimisticTransactionOptions, Transaction, TransactionDB,
        TransactionDBOptions, TransactionOptions,
    },
    write_batch::{WriteBatch, WriteBatchIterator, WriteBatchWithTransaction},
};

use librocksdb_sys as ffi;

use std::error;
use std::fmt;

/// RocksDB error kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    NotFound,
    Corruption,
    NotSupported,
    InvalidArgument,
    IOError,
    MergeInProgress,
    Incomplete,
    ShutdownInProgress,
    TimedOut,
    Aborted,
    Busy,
    Expired,
    TryAgain,
    CompactionTooLarge,
    ColumnFamilyDropped,
    Unknown,
}

/// A simple wrapper round a string, used for errors reported from
/// ffi calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    message: String,
}

impl Error {
    fn new(message: String) -> Error {
        Error { message }
    }

    pub fn into_string(self) -> String {
        self.into()
    }

    /// Parse corresponding [`ErrorKind`] from error message.
    pub fn kind(&self) -> ErrorKind {
        match self.message.split(':').next().unwrap_or("") {
            "NotFound" => ErrorKind::NotFound,
            "Corruption" => ErrorKind::Corruption,
            "Not implemented" => ErrorKind::NotSupported,
            "Invalid argument" => ErrorKind::InvalidArgument,
            "IO error" => ErrorKind::IOError,
            "Merge in progress" => ErrorKind::MergeInProgress,
            "Result incomplete" => ErrorKind::Incomplete,
            "Shutdown in progress" => ErrorKind::ShutdownInProgress,
            "Operation timed out" => ErrorKind::TimedOut,
            "Operation aborted" => ErrorKind::Aborted,
            "Resource busy" => ErrorKind::Busy,
            "Operation expired" => ErrorKind::Expired,
            "Operation failed. Try again." => ErrorKind::TryAgain,
            "Compaction too large" => ErrorKind::CompactionTooLarge,
            "Column family dropped" => ErrorKind::ColumnFamilyDropped,
            _ => ErrorKind::Unknown,
        }
    }
}

impl AsRef<str> for Error {
    fn as_ref(&self) -> &str {
        &self.message
    }
}

impl From<Error> for String {
    fn from(e: Error) -> String {
        e.message
    }
}

impl error::Error for Error {
    fn description(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        self.message.fmt(formatter)
    }
}
