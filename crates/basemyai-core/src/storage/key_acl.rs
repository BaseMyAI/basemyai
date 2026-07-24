// SPDX-License-Identifier: BUSL-1.1
//! Windows ACL hardening for the default encryption key file/directory
//! (CRYPTO-2, BaseMyAI adversarial audit, 2026-07-22).
//!
//! `key.rs`'s Unix path restricts `~/.basemyai` (0700) and `~/.basemyai/key`
//! (0600) via mode bits and actively re-validates them on every read
//! (`validate_default_key_permissions`). Before this module existed, the
//! Windows side of both functions was a genuine no-op
//! (`create_dir_all`/`fs::write` unconditionally, permission validation
//! skipped entirely) — a generated key persisted with whatever default or
//! inherited ACL NTFS happened to assign, with no verification.
//!
//! [`restrict_to_current_user`] replaces an object's DACL with a single ACE
//! granting the *current process's user SID* full control, and marks the
//! DACL `PROTECTED` — the flag that actually blocks inherited ACEs from the
//! parent directory from applying. Without `PROTECTED`, `SetNamedSecurityInfoW`
//! would just add this ACE alongside whatever the parent already grants,
//! which would not restrict anything a looser parent ACL already allowed.
//!
//! [`is_restricted_to_current_user`] reads the DACL back and confirms it
//! still holds exactly the one ACE this module writes — the same "any
//! extra bit is a failure" posture as the Unix side's `mode & 0o077 != 0`
//! check, adapted to ACLs (an exact-shape comparison, not a general
//! "is this ACL definitely fine" audit).

use std::ffi::c_void;
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GetNamedSecurityInfoW, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW,
    TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, DACL_SECURITY_INFORMATION, EqualSid,
    GetAce, GetAclInformation, GetLengthSid, GetTokenInformation, NO_INHERITANCE, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::core::PWSTR;

/// A Win32 ABI constant stable since the original security descriptor
/// design (`WinNT.h`/`AccCtrl.h`) — not exposed as a named item in this
/// `windows-sys` version's `Security` module, so declared locally rather
/// than left as a bare literal at the use site.
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

fn wide_null_terminated(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// The current process's user SID, as an owned byte buffer (a `SID` is a
/// variable-length structure; owning the bytes keeps the `PSID` pointer we
/// hand to Win32 APIs valid for as long as this value lives, rather than
/// pointing into a token-information buffer that has already been freed).
pub(super) struct OwnedSid(Vec<u8>);

impl OwnedSid {
    fn as_psid(&self) -> PSID {
        // SAFETY: `self.0` was populated by `GetTokenInformation` as a
        // complete, well-formed `SID` (verified by that call succeeding)
        // and is never mutated after construction — casting its start to
        // `PSID` (an opaque pointer type Win32 APIs treat as `*mut SID`)
        // matches exactly how `GetTokenInformation` itself populated it in
        // place.
        self.0.as_ptr().cast_mut().cast::<c_void>()
    }
}

struct CloseHandleOnDrop(HANDLE);
impl Drop for CloseHandleOnDrop {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the valid, open handle this guard was
        // constructed with; closed at most once, only here.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct LocalFreeOnDrop(*mut c_void);
impl Drop for LocalFreeOnDrop {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` was allocated by a Win32 API documented to
            // return a `LocalAlloc`-family block (`SetEntriesInAclW`'s
            // output ACL, or `GetNamedSecurityInfoW`'s output security
            // descriptor) and is freed at most once, only here, only after
            // every use of it elsewhere in the owning function has
            // completed.
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}

/// Reads the current process's user SID via `OpenProcessToken` +
/// `GetTokenInformation(TokenUser)` — the standard two-call sizing pattern
/// (`GetTokenInformation` is asked for a length via a zero-size buffer
/// first, matching every Win32 API in this family).
///
/// # Errors
/// A [`io::Error`] built from `GetLastError()` if opening the process token
/// or reading its user SID fails — never a panic; the caller (`key.rs`)
/// treats this as "ACL hardening unavailable", not a hard failure of key
/// persistence itself.
pub(super) fn current_user_sid() -> io::Result<OwnedSid> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no
    // closing; `OpenProcessToken` is called with a valid process handle and
    // an out-pointer to a stack-local `HANDLE`, per its documented
    // contract.
    let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) };
    if opened == 0 {
        return Err(io::Error::last_os_error());
    }
    let _token_guard = CloseHandleOnDrop(token);

    let mut needed: u32 = 0;
    // SAFETY: a null buffer with `tokeninformationlength = 0` is the
    // documented way to ask `GetTokenInformation` for the required buffer
    // size via `needed`; it is expected to fail (typically
    // `ERROR_INSUFFICIENT_BUFFER`), which this call intentionally ignores —
    // only `needed` is consulted.
    unsafe {
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &raw mut needed);
    }
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }

    let mut buffer = vec![0u8; needed as usize];
    let mut actual: u32 = 0;
    // SAFETY: `buffer` is sized exactly to `needed` (the size the previous
    // call reported), a valid, uniquely-owned, sufficiently-aligned
    // allocation for `GetTokenInformation` to write a `TOKEN_USER` into
    // (Win32 guarantees byte-buffer outputs meet natural alignment for the
    // structure requested).
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast::<c_void>(),
            needed,
            &raw mut actual,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: `buffer` was just populated by the successful call above as a
    // `TOKEN_USER` — reading it as that type is exactly what
    // `GetTokenInformation(TokenUser)`'s contract promises. `User.Sid`
    // points *into* `buffer` itself (Win32 packs the SID bytes after the
    // struct, in the same allocation) — dereferencing it here, before
    // `buffer` is touched again, is sound; the SID bytes are copied out
    // (`sid_bytes`) before `buffer` is dropped, so nothing borrows into it
    // afterward.
    let sid_ptr = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    // SAFETY: `sid_ptr` was just read from a `TOKEN_USER` Win32 itself
    // populated in `buffer` above — passing it to `GetLengthSid` (which
    // only reads the SID's own header bytes to compute its total length)
    // is exactly its documented use.
    let sid_len = unsafe { GetLengthSid(sid_ptr) } as usize;
    if sid_len == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `sid_ptr` was just validated non-degenerate by
    // `GetLengthSid` returning a nonzero length, and still points inside
    // `buffer` (not yet dropped) — copying exactly `sid_len` bytes out is
    // the documented way to take ownership of a `SID`'s variable-length
    // representation.
    let sid_bytes = unsafe { std::slice::from_raw_parts(sid_ptr.cast::<u8>(), sid_len) }.to_vec();
    Ok(OwnedSid(sid_bytes))
}

/// Replaces `path`'s DACL with a single ACE granting `sid` full control,
/// `PROTECTED` (blocks inherited ACEs from the parent from also applying —
/// see the module doc for why this flag is load-bearing, not decorative).
///
/// # Errors
/// A [`io::Error`] if building or applying the ACL fails; never a panic.
/// The caller treats this as "ACL hardening unavailable" for this path, the
/// same posture as [`current_user_sid`]'s failure mode.
pub(super) fn restrict_to_current_user(path: &Path, sid: &OwnedSid) -> io::Result<()> {
    let mut wide_path = wide_null_terminated(path);

    // SAFETY: `zeroed()` is a valid initial state for `TRUSTEE_W` (its own
    // `Default` impl does exactly this) — every field this function relies
    // on is explicitly set immediately below before the value is used.
    let mut trustee: TRUSTEE_W = unsafe { std::mem::zeroed() };
    trustee.TrusteeForm = TRUSTEE_IS_SID;
    trustee.TrusteeType = TRUSTEE_IS_USER;
    // `ptstrName` doubles as the `PSID` slot when `TrusteeForm ==
    // TRUSTEE_IS_SID` (documented Win32 union-by-convention for this
    // struct) — `sid` outlives this whole function, so the pointer stays
    // valid for every call below.
    trustee.ptstrName = sid.as_psid().cast::<u16>();

    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: GENERIC_ALL,
        grfAccessMode: SET_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: trustee,
    };

    let mut new_acl: *mut ACL = std::ptr::null_mut();
    // SAFETY: `entry` is a single, fully-initialized `EXPLICIT_ACCESS_W`
    // (count `1` matches exactly); `new_acl` is a valid out-pointer to a
    // stack-local `*mut ACL`, per `SetEntriesInAclW`'s documented contract.
    // On success, `new_acl` is heap-allocated by Win32 (`LocalAlloc`
    // internally) — freed via the guard below once no longer needed.
    let status = unsafe { SetEntriesInAclW(1, &raw const entry, std::ptr::null_mut(), &raw mut new_acl) };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let _acl_guard = LocalFreeOnDrop(new_acl.cast::<c_void>());

    // SAFETY: `wide_path` is a valid, null-terminated UTF-16 buffer this
    // function owns for the duration of the call; `new_acl` is the ACL just
    // built above, still valid (freed only by the guard, after this call
    // returns). `PROTECTED_DACL_SECURITY_INFORMATION` is included precisely
    // so this DACL replaces, rather than merely supplements, whatever the
    // parent directory would otherwise contribute by inheritance.
    let status = unsafe {
        SetNamedSecurityInfoW(
            wide_path.as_mut_ptr() as PWSTR,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new_acl,
            std::ptr::null_mut(),
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
}

/// Reads `path`'s DACL back and confirms it holds exactly one ACE, of type
/// `ACCESS_ALLOWED`, granting `sid` (only) `GENERIC_ALL` — precisely the
/// shape [`restrict_to_current_user`] writes. Any deviation (a null/absent
/// DACL — "everyone has full access" — more than one ACE, a different
/// grantee, or a narrower mask) returns `false`, never silently `true`.
///
/// # Errors
/// A [`io::Error`] if the DACL itself cannot be read at all (path missing,
/// access denied to query security information) — distinct from `Ok(false)`,
/// which means "read successfully, but not in the expected restricted
/// shape".
pub(super) fn is_restricted_to_current_user(path: &Path, sid: &OwnedSid) -> io::Result<bool> {
    let wide_path = wide_null_terminated(path);
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut security_descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `wide_path` is a valid null-terminated UTF-16 buffer;
    // `dacl`/`security_descriptor` are valid out-pointers to stack-local
    // locals, per `GetNamedSecurityInfoW`'s documented contract. On
    // success, `security_descriptor` is heap-allocated by Win32 and owns
    // the memory `dacl` points into — freed via the guard below, which
    // must outlive every use of `dacl`.
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide_path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut dacl,
            std::ptr::null_mut(),
            &raw mut security_descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let _sd_guard = LocalFreeOnDrop(security_descriptor);

    if dacl.is_null() {
        // A null DACL means no discretionary access control at all —
        // everyone has full access. Never treated as restricted.
        return Ok(false);
    }

    let mut size_info: ACL_SIZE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `dacl` was just validated non-null and was populated by the
    // successful `GetNamedSecurityInfoW` call above; `size_info` is a
    // correctly-sized, aligned out-buffer for `AclSizeInformation`, per
    // `GetAclInformation`'s documented contract.
    let ok = unsafe {
        GetAclInformation(
            dacl,
            (&raw mut size_info).cast::<c_void>(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if size_info.AceCount != 1 {
        return Ok(false);
    }

    let mut ace_ptr: *mut c_void = std::ptr::null_mut();
    // SAFETY: `dacl` is the same validated ACL as above, with exactly one
    // ACE (just confirmed) — index `0` is in bounds. `GetAce` returns a
    // pointer *into* `dacl`'s own buffer (owned by `security_descriptor`,
    // freed only by the guard above, after this function returns) — no
    // separate lifetime to manage.
    let ok = unsafe { GetAce(dacl, 0, &raw mut ace_ptr) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: every ACE begins with an `ACE_HEADER` (Win32 ABI guarantee);
    // `ace_ptr` was just returned by `GetAce` as a valid pointer to one.
    let header = unsafe { &*ace_ptr.cast::<ACE_HEADER>() };
    if header.AceType != ACCESS_ALLOWED_ACE_TYPE {
        return Ok(false);
    }
    // SAFETY: `header.AceType == ACCESS_ALLOWED_ACE_TYPE` (just checked)
    // guarantees the bytes at `ace_ptr` are laid out as `ACCESS_ALLOWED_ACE`
    // (header + mask + a trailing SID starting at `SidStart`'s offset) —
    // exactly the type Win32 documents for this ACE type.
    let ace = unsafe { &*ace_ptr.cast::<ACCESS_ALLOWED_ACE>() };
    // `SidStart` is the documented zero-sized marker for where the
    // variable-length SID trailing this struct begins (the standard Win32
    // idiom for `ACCESS_ALLOWED_ACE`) — its address, not its value, is the
    // SID's start.
    let ace_sid: PSID = (&raw const ace.SidStart).cast::<c_void>().cast_mut();
    // SAFETY: `ace_sid` points at a SID living inside `dacl`'s buffer
    // (still valid — the guard has not run yet); `sid.as_psid()` points at
    // `sid`'s own owned buffer, alive for this whole function call.
    // `EqualSid` only reads both, never writes.
    let equal = unsafe { EqualSid(ace_sid, sid.as_psid()) };
    // `ace.Mask` is deliberately *not* compared against `GENERIC_ALL`: Win32
    // normalizes a generic right into the object type's specific rights the
    // moment the ACE is actually stored (e.g. `FILE_ALL_ACCESS`, not the
    // `GENERIC_ALL` bit itself survives a round trip) — the exact numeric
    // value is an implementation detail of that mapping, not something this
    // function wrote or controls. What matters for "restricted", and what
    // this checks, is that the sole ACE grants *some* access to *exactly*
    // the expected SID and nothing else — a wrong or additional grantee is
    // what `AceCount != 1`/`EqualSid` above already catch.
    Ok(equal != 0 && ace.Mask != 0)
}

/// Test-only: the well-known "Everyone" SID (`S-1-1-0`), a stable,
/// documented byte layout (MS-DTYP §2.4.2 — revision 1, one sub-authority,
/// identifier authority `{0,0,0,0,0,1}`, sub-authority `0`) — used to
/// simulate an over-permissive ACL for [`is_restricted_to_current_user`]'s
/// negative-case regression test, without needing `CreateWellKnownSid`.
#[cfg(test)]
pub(super) fn everyone_sid_for_test() -> OwnedSid {
    OwnedSid(vec![1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0])
}
