# strand/ — Immutable Small-String-Optimised String

`surrealdb-strand` is a single-type crate exposing [`Strand`], an immutable
24-byte string used everywhere the value layer stores string-shaped data:
`Value::String`, `Object` keys, `TableName`, `RecordIdKey::String`. It trades
`String`'s mutability for three storage strategies picked at construction time,
and a wire format that is **byte-identical to `String`** so it is a drop-in
on-disk and on-index replacement.

The entire implementation is in
`analayse/surrealdb/surrealdb/strand/src/lib.rs` (~710 lines + ~440 lines of
tests); benchmarks live in `analayse/surrealdb/surrealdb/strand/benches/strand.rs`.

## Why it exists

A database value layer compares, orders, clones, and hashes *enormous* numbers
of small strings (record-id keys, object field names, table names, reserved
keywords). For those, `String` is wasteful:

- a heap allocation per value, even for `"id"`;
- `clone()` is always `malloc + memcpy`;
- compile-time-known literals (keywords, response keys) re-allocate at runtime;
- comparison dereferences a pointer before touching any bytes.

`Strand` removes the allocation for short and compile-time-known strings and
makes the common comparison path branchless.

## The three variants

| Variant | Backing | Construction | `clone()` | `drop()` | Use case |
|---------|---------|--------------|-----------|----------|----------|
| **Inline** | 23 bytes on the stack | `Strand::from(&str)` / `new` when `len ≤ 23` | bitwise copy | no-op | short dynamic strings (keys, ids) |
| **Static** | `&'static str` ptr | `Strand::new_static` (`const`) | pointer copy | no-op | compile-time literals of *any* length |
| **Boxed** | `Box<str>` | `from` when `len > 23` | `malloc + memcpy` | frees | long dynamic strings |

`INLINE_CAP = 23`. `Static` is the highest-leverage variant: it is `const`,
never allocates regardless of length, and clones as a bitwise pointer copy —
ideal for the long-but-fixed strings (`"geometry<multipolygon>"`, reserved
keywords) that a naive design would heap-allocate on every use.

## Layout — the 24-byte union

```
        byte 0 ─────────────────────────────────► byte 23
Inline: [ u t f 8   d a t a  . . . . . . . . . . ][ len ]   tag = 0..=23
Static: [ ptr: *const u8 ][ len: usize ][ pad   ][ 254 ]   tag = TAG_STATIC
Boxed:  [ ptr: *const u8 ][ len: usize ][ pad   ][ 255 ]   tag = TAG_BOXED
```

```rust
#[repr(C)]
struct HeapData { ptr: *const u8, len: usize, _pad: [u8; 23 - 2*size_of::<usize>()], tag: u8 }

#[repr(C)]
union StrandData { inline: [u8; 24], heap: ManuallyDrop<HeapData> }

#[repr(transparent)]
pub struct Strand { data: StrandData }
```

The 24th byte (index 23) is the **discriminant tag**, deliberately overlapping
the last byte of `HeapData` (`tag: u8`). This is the whole trick:

- For `Inline`, byte 23 holds the **length** (`0..=23`), so a single byte
  serves as both discriminant *and* length.
- For `Static`/`Boxed`, the first 16 bytes (on 64-bit) form a valid `&str` fat
  pointer; byte 23 is `254`/`255`.

`size_of::<Strand>() == 24` — identical to `String` on 64-bit, asserted by
`stack_size_is_24_bytes`.

## Branchless `as_str()`

`as_str()` is the hot path behind `Deref`, equality, ordering, hashing, and
serialisation. It reads the tag, then tells LLVM the gap `24..=253` is
impossible:

```rust
let tag = self.data.inline[23];
if tag > INLINE_CAP as u8 && tag != TAG_STATIC && tag != TAG_BOXED {
    std::hint::unreachable_unchecked();           // collapses the branch table
}
let is_inline = tag <= INLINE_CAP as u8;
let len = if is_inline { tag as usize } else { self.data.heap.len };
let ptr = if is_inline { self.data.inline.as_ptr() } else { self.data.heap.ptr };
std::str::from_utf8_unchecked(std::slice::from_raw_parts(ptr, len))
```

Because the inline length and the heap length both live at predictable offsets,
the compiler lowers `len`/`ptr` selection to conditional moves rather than
branches.

## Fast paths that matter

- **`PartialEq`** — if both operands are inline, compare the full **24-byte
  arrays** directly (`self.data.inline == other.data.inline`). This is sound
  *only because* `new_inline` zero-initialises the whole buffer first, so
  padding bytes are guaranteed zero. One SIMD-friendly 24-byte compare, no
  length branch, no pointer chase.
- **`Ord`** — if both inline, slice to the exact tagged length and `cmp` the
  bytes. Padding must **not** be included here (unlike `eq`) because trailing
  zeros would corrupt lexicographic order.
- **`Clone`** — bitwise `ptr::read` for `Inline`/`Static`; the `Boxed` branch
  is marked `#[cold] #[inline(never)]` so the allocator call never bloats the
  hot path.

## Wire-format compatibility (the load-bearing invariant)

`Strand` implements `revision::{Serialize,Deserialize}Revisioned` and
`storekey::{Encode,Decode,BorrowDecode}` to produce **byte-identical** output
to `String` for the same input. This is what makes it a safe drop-in: existing
on-disk documents, change feeds, and index keys decode unchanged after the
type swap. Any divergence would silently corrupt persisted data on upgrade.

The test suite guards this hard:

- `revisioned_wire_matches_string` / `storekey_wire_matches_string` assert
  byte-identity **and** cross-type decode (Strand decodes String's bytes and
  vice-versa) across every structural case: empty, `INLINE_CAP ± 1`, long
  ASCII, multi-byte UTF-8 straddling the boundary, and payloads with `0x00`/
  `0x01` escape bytes.
- The `DeserializeRevisioned` impl decodes **in place** into the inline buffer
  (≤ cap) or a fresh `Vec` (> cap), bypassing `String` entirely, and validates
  UTF-8 even on the direct-write path.

## `unsafe` surface — what makes it sound

All `unsafe` rests on one controlled invariant: **byte 23 is only ever set to
`0..=23`, `254`, or `255`**, and inline buffers are fully zero-initialised
before use.

| Site | Safety argument |
|------|-----------------|
| `new_inline` | length checked `≤ INLINE_CAP`; buffer zeroed before copy |
| `as_str` | tag is one of the three controlled ranges → `unreachable_unchecked` valid |
| `Drop` | frees `Box<str>` **only** when `tag == TAG_BOXED`; ptr/len came from a real `Box` via `From<Box<str>>` + `mem::forget` |
| `Clone` | bitwise copy only for non-owning variants; Boxed deep-copies |
| `PartialEq` | full-array compare only when both inline → padding provably zero |
| `Send`/`Sync` | manual impls; `Static`/`Boxed` hold an immutable `*const u8`, never mutated |

`from_display` is a notable extra: it formats *directly into* the inline buffer
via a custom `fmt::Write`, only spilling to a heap `String` on overflow — zero
allocation for short formatted keys/ids.

## Dependencies & features

- `revision`, `serde`, `storekey` (all `workspace = true`) — the three wire
  formats `Strand` must mirror.
- `arbitrary` (optional, feature `arbitrary`) — fuzzing.
- `criterion` (dev) — the `strand` bench (`harness = false`).
- `[lints] workspace = true` — inherits the workspace lint policy.

## Reading order

1. Module docs + `Layout` section in `strand/src/lib.rs` — the design rationale.
2. `as_str()` (line ~222) — the branchless core every other impl rests on.
3. `PartialEq` / `Ord` — the inline fast paths.
4. The `revision`/`storekey` impls + the `*_wire_matches_string` tests — the
   drop-in invariant.
5. `strand/benches/strand.rs` — `Strand` vs `String` for eq/cmp/clone
   across all four size classes (static, short, at-cap, long).
