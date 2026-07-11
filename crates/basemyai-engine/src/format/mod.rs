// SPDX-License-Identifier: BUSL-1.1
//! Versioned on-disk record layouts (Layer 1).
//!
//! Every persisted type in this module carries an explicit version constant
//! (`WAL_RECORD_VERSION`, `SST_FORMAT_VERSION`) paired with a documented byte
//! layout. That pairing is what the `format.lock` mechanism (built in
//! parallel, elsewhere) hashes against: bump the constant *and* update the
//! doc comment together whenever a layout changes — never silently.

pub(crate) mod checksum;
pub(crate) mod crypto;
pub mod lock;
pub mod sst_block;
pub mod store_meta;
pub mod wal;

pub use lock::FormatSpec;
