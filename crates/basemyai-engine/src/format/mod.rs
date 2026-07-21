// SPDX-License-Identifier: BUSL-1.1
//! Versioned on-disk record layouts (Layer 1).
//!
//! Every persisted type in this module carries an explicit version constant
//! (`WAL_RECORD_VERSION`, `SST_FORMAT_VERSION`) paired with a documented byte
//! layout. That pairing is what the `format.lock` mechanism (built in
//! parallel, elsewhere) hashes against: bump the constant *and* update the
//! doc comment together whenever a layout changes — never silently.

pub(crate) mod checksum;
// `pub`, not `pub(crate)`: the module is otherwise self-contained encoding
// (see the module doc), but its three `decode_*` fns need to be reachable
// from the external `fuzz/` crate — same reasoning that already made every
// other decoder in this module (`sst_block`, `store_meta`, `wal`) `pub`.
pub mod crypto;
pub mod generation_meta;
pub mod lock;
pub mod sst_block;
pub mod sst_manifest;
pub mod store_meta;
pub mod wal;

pub use lock::FormatSpec;
