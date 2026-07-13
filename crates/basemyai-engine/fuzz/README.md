# `basemyai-engine` fuzz targets

Standard `cargo-fuzz` layout (`fuzz/Cargo.toml` + `fuzz/fuzz_targets/*.rs`),
targeting the decode paths in `basemyai-engine`'s persisted formats
(`docs/adr/ADR-025-native-engine-storage-foundation.md`, N2 item in
`docs/TODO-NATIVE-ENGINE.md`: "Fuzzing cargo-fuzz (nightly séparée) :
encodage/décodage clés, replay WAL, parsing pages").

**Deliberately not part of the workspace and not run by `cargo xtask
check`/`test`/`ci`.** This crate needs a **nightly** toolchain plus the
`cargo-fuzz` subcommand (libFuzzer) — the default CI matrix only has stable.
`fuzz/Cargo.toml` carries its own empty `[workspace]` table so `cargo`
commands run from the repo root never pull it in, and `fuzz/rust-toolchain.toml`
pins `nightly` for anything run with a working directory inside `fuzz/` (an
override closer to the CWD than the repo-root `rust-toolchain.toml`, which
pins stable `1.95` for everything else).

## Platform note: this does not run on native Windows

`cargo-fuzz`/libFuzzer needs a sanitizer (ASan) runtime that isn't wired up
for the `x86_64-pc-windows-msvc` target. As of 2026-07-12, `cargo +nightly
fuzz build` (every target) actually **succeeds** — link errors like the
historical `LINK : fatal error LNK1561` are no longer reproduced on this
toolchain/cargo-fuzz version — but running the resulting binary fails at
**process start**, not link time: `STATUS_DLL_NOT_FOUND` (exit code
`0xc0000135`), because the ASan runtime DLL the instrumented binary needs
isn't present (no LLVM/clang-cl install ships it on Windows, and rustc's own
sysroot doesn't bundle it for this target either — both checked directly,
not assumed). Net effect: **you cannot get a real fuzzing run out of native
Windows**, only confirm targets build. **Run actual fuzzing on Linux, macOS,
or WSL2** — that's exactly what happened on 2026-07-12: a freshly reinstalled
Kali-under-WSL instance was provisioned from scratch (rustup nightly minimal
profile, `build-essential`/`clang`/`llvm` via apt, `cargo-fuzz` via `cargo
install`) and all 24 targets were run for real from there, `-max_total_time
=30` each. See per-target notes below and the summary after the target list
for results (zero crashes across all 24).

## Targets

- **`key_roundtrip`** — `basemyai_engine::Key::from`/`as_bytes`/`into_bytes`
  never panic on arbitrary bytes, and the byte round-trip holds. `Key` has no
  encoding of its own yet (thin byte-ordered wrapper, `src/key/mod.rs`), so
  there's no decode-from-untrusted-bytes path to attack today — this target
  is here so it's trivial to extend the moment this crate grows a real key
  encoder (varint length prefixes, entity tags, etc.).
- **`wal_decode`** — `format::wal::decode` on arbitrary/malformed byte
  streams, mirroring the shape of `store::wal::Wal::replay`'s loop (decode,
  advance by `consumed`, stop on `None`/`Err`). Asserts forward progress on
  every `Some(..)` so a decoder bug that returns `consumed == 0` shows up as
  a fuzzer timeout/panic instead of silently wedging replay.
- **`sst_decode`** / **`sst_decode_structured`** — **retired** (ADR-039/N8.5):
  targeted `format::sst::decode`, the whole-file `SstFile:1` format's
  decoder. That module was deleted when the block-based SST format (ADR-039)
  replaced it outright (no dual-format transition, per that ADR's §5.3
  policy) — there is nothing left to fuzz. The entry-count bounding lesson
  those targets found (see "Known finding" below) carries forward: every
  `format::sst_block` decoder bounds attacker-controlled counts against the
  buffer's actual remaining length before any `Vec::with_capacity`, and
  `sst_data_block_decode`/`sst_data_block_decode_structured` below are its
  direct successors.
- **`vector_node_decode`** — raw arbitrary bytes into
  `idx::vector::node::decode` (the LM-DiskANN node block, ADR-026). Same
  crc32-gate caveat as `sst_decode`.
- **`vector_meta_decode`** — raw arbitrary bytes into
  `idx::vector::meta::decode` (the index metadata record; fixed-length, so
  the structural surface is small).
- **`vector_node_decode_structured`** — the `sst_decode_structured`
  counterpart for the **v2** node block (N3 deletes step: `flags` byte with
  the tombstone bit): header with controlled version/flags/dim/
  neighbor_count + arbitrary body + *correct* trailing crc32, so the fuzzer
  explores the post-checksum surface (reserved-flag-bits rejection, lying
  counts vs the exact-length equation). **Executed** (WSL/Kali, 2026-07-12,
  see below) — zero crashes.
- **`graph_entity_decode`** (N4) — raw arbitrary bytes into
  `idx::graph::entity::decode` (the graph-entity block). Same crc32-gate
  caveat as `sst_decode`/`vector_node_decode`. **Executed** — zero crashes.
- **`graph_edge_decode`** (N4) — raw arbitrary bytes into
  `idx::graph::edge::decode` (the graph-edge record; fixed-length, small
  structural surface like `vector_meta_decode`). **Executed** — zero crashes.
- **`sst_header_decode`** (N8.2, ADR-039) — raw arbitrary bytes into
  `format::sst_block::decode_sst_header`. Fixed-length, small structural
  surface like `vector_meta_decode` (plus the `block_size != 0` gate).
  **Executed** — zero crashes.
- **`sst_data_block_decode`** / **`sst_data_block_decode_structured`** (N8.2)
  — the block-based-SST-format siblings of `sst_decode`/
  `sst_decode_structured`: one data block (`format::sst_block::SstDataBlock`)
  instead of the whole legacy file, same `entry_count`-bounding bug class the
  structured variant exists to catch. **Executed** — zero crashes.
- **`sst_block_index_decode`** / **`sst_block_index_decode_structured`**
  (N8.2) — same pattern against `decode_sst_block_index`
  (`format::sst_block::SstBlockIndex`), whose per-entry `first_key_len`/
  `last_key_len` are the wire-controlled lengths at risk. **Executed** —
  zero crashes.
- **`sst_bloom_filter_decode`** (N8.2) — raw arbitrary bytes into
  `decode_sst_bloom_filter` (`format::sst_block::SstBloomFilter`), whose
  `bits_len` is cross-checked against `ceil(num_bits / 8)` before slicing.
  **Executed** — zero crashes.
- **`sst_footer_decode`** (N8.2) — raw arbitrary bytes into
  `decode_sst_footer` (`format::sst_block::SstFooter`). Fixed-length, small
  structural surface like `vector_meta_decode` (plus the trailing
  `footer_magic` sentinel check). **Executed** — zero crashes.
- **`store_meta_decode`** (N8.2, ADR-039 §7) — raw arbitrary bytes into
  `format::store_meta::decode`. Fixed-length, small structural surface like
  `vector_meta_decode`. **Executed** — zero crashes.
- **`crypto_meta_decode`** / **`wal_envelope_decode`** /
  **`encrypted_sst_block_decode`** (N11 §8.4) — the three encryption-at-rest
  decoders (ADR-030/ADR-039 §3): `crypto.meta` key-wrap, the per-record WAL
  envelope, and the per-section `EncryptedSstBlock` envelope. Their real
  decoders (`format::crypto::decode_{crypto_meta,wal_envelope,
  encrypted_sst_block}`) stay `pub(crate)` — their return types
  (`CryptoMeta`/`Nonce`/`WalEnvelopeRef`) are deliberately crate-private, so
  these targets go through thin `pub fn fuzz_decode_*` shims added
  specifically for this (`format/crypto.rs`) rather than widening the
  crate's public API. **Executed** — zero crashes.
- **`fts_docterms_decode`** (N11 §8.4) — raw arbitrary bytes into
  `idx::fts::docterms::decode` (ADR-028 §3). `count` and per-entry
  `term_len` are the wire-controlled fields at risk, same bounding
  discipline as `sst_data_block_decode_structured`. **Executed** — zero
  crashes.
- **`fts_postings_decode`** / **`fts_stats_decode`** (N11 §8.4) — raw
  arbitrary bytes into `idx::fts::postings::decode` /
  `idx::fts::stats::decode`. Both fixed-length, small structural surface
  like `vector_meta_decode`. **Executed** — zero crashes.
- **`memory_index_meta_decode`** (N11 §8.4) — raw arbitrary bytes into
  `idx::memory::meta::decode`, the persisted `next_vec_id` allocator
  (ADR-027 §4). Fixed-length, small structural surface — but a decode bug
  here that silently under-reports would risk reusing a live `vec_id`, so
  it earns its own target despite the small surface. **Executed** — zero
  crashes.
- **`memory_record_decode`** (N11 §8.4) — raw arbitrary bytes into
  `idx::memory::record::decode`, the **primary** memory-record block
  (ADR-027 §2) — unlike every derived structure this crate can rebuild from
  data, nothing else can reconstruct a lost/corrupted one (ADR-040 §1).
  `layer_len`/`content_len`/`source_len` are the wire-controlled fields at
  risk. **Executed** — zero crashes.
- **`memory_vecmap_decode`** (N11 §8.4) — raw arbitrary bytes into
  `idx::memory::vecmap::decode`, the `vec_id -> (agent, id)` reverse mapping
  (ADR-027 §2/§4). `agent_len`/`id_len` are the wire-controlled fields at
  risk. **Executed** — zero crashes.

All 24 targets (the nine new ones from this session plus the 15 pre-existing)
were run for real under WSL/Kali on 2026-07-12 — `cargo fuzz run <target> --
-max_total_time=30` each, from a few million to tens of millions of
executions per target depending on corpus size. **Zero crashes, zero panics,
zero timeouts across all 24**, `exit=0` confirmed for every one. Every
decoder in this crate now has an executed fuzz target, closing the gap
`docs/PLAN-NATIVE-ENGINE.md` §8.4 called out.

## Known finding (historical, in code deleted by ADR-039/N8.5)

`format::sst::decode` — the whole-file `SstFile:1` decoder, deleted along
with the rest of `format/sst.rs` and `store/sst.rs` when the block-based SST
format replaced it outright (ADR-039 §5.3, no dual-format transition) — used
to read the file's `entry_count: u64` header field and pass it straight to
`Vec::with_capacity(entry_count as usize)` **before** checking it against
the buffer's actual remaining length. A crafted 18-byte file — magic +
version + `entry_count = u64::MAX` + a correctly-computed trailing crc32 —
panicked with `capacity overflow` instead of returning
`EngineError::CorruptSst`. The now-retired `sst_decode_structured` target
reproduced this in well under a second of fuzzing. The lesson carried
forward directly: every `format::sst_block` decoder
(`decode_sst_data_block`, `decode_sst_block_index`, ...) bounds every
attacker-controlled count against `(buffer_len - fixed_header) /
min_entry_size` **before** any `Vec::with_capacity` call — see
`sst_data_block_decode_structured`/`sst_block_index_decode_structured` for
the fuzz coverage of that discipline in the current format.

Crash artifacts are not committed (`artifacts/` and `corpus/` are
git-ignored, they're machine/run-specific) — rerun as below to reproduce
findings on the current targets.

## Running locally

```bash
# One-time setup (Linux/macOS/WSL only):
rustup toolchain install nightly
cargo install cargo-fuzz --locked

# From crates/basemyai-engine/fuzz (its rust-toolchain.toml auto-selects
# nightly), or from crates/basemyai-engine with `cargo +nightly fuzz ...`:
cd crates/basemyai-engine/fuzz
cargo fuzz list
cargo fuzz run key_roundtrip -- -max_total_time=30
cargo fuzz run wal_decode -- -max_total_time=30
cargo fuzz run sst_data_block_decode_structured -- -max_total_time=30

# Reproduce a saved crash:
cargo fuzz run <target> artifacts/<target>/<crash-file>
```

## CI

Not wired into `.github/workflows/ci.yml`. A dedicated nightly-toolchain CI
job (e.g. a scheduled/nightly-cron job, matching the `embed`/`crypto` job
pattern already in `ci.yml`, each running `cargo fuzz run <target> --
-max_total_time=<n>` per target) would be a reasonable follow-up, but adding
CI YAML wasn't done here — flagging it for a human to decide the right
cadence (every push is likely too slow/noisy for a fuzzer; nightly-scheduled
is the common pattern) rather than guessing.
