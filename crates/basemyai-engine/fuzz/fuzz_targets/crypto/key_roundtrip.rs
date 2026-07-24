//! Fuzz target: `Key` construction/round-trip from arbitrary bytes.
//!
//! `basemyai_engine::key::Key` has no encoding of its own today (it's a thin
//! byte-ordered wrapper, see `src/key/mod.rs`), so there is no decode-from-
//! untrusted-bytes path to attack yet. This target still earns its keep:
//! it pins down that `Key::from`/`as_bytes`/`into_bytes` never panic on any
//! input (including empty and non-UTF8 byte strings) and that the
//! byte-round-trip invariant (`Key::from(bytes).as_bytes() == bytes`) holds,
//! so a regression the moment this crate grows a real key encoder (varint
//! length prefixes, entity tags, etc.) is caught immediately by extending
//! this same target instead of writing a new one from scratch.

#![no_main]

use basemyai_engine::Key;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let key = Key::from(data);
    assert_eq!(key.as_bytes(), data);

    // Ord/Eq must never panic regardless of content.
    let other = Key::from(data);
    assert_eq!(key, other);

    let owned = key.into_bytes();
    assert_eq!(owned, data);
});
