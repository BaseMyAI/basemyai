// SPDX-License-Identifier: BUSL-1.1
//! Minimal CRC32 (IEEE 802.3, reflected polynomial) — zero external
//! dependency. Used to detect torn/corrupt writes in WAL records and SST
//! files. Internal to the crate: not itself a persisted "type" in the
//! `format.lock` sense, just a helper the persisted formats use.

use std::sync::OnceLock;

const POLY: u32 = 0xEDB8_8320;

static TABLE: OnceLock<[u32; 256]> = OnceLock::new();

fn table() -> &'static [u32; 256] {
    TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { POLY ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        table
    })
}

pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    let table = table();
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        let idx = ((crc ^ u32::from(byte)) & 0xFF) as usize;
        crc = table[idx] ^ (crc >> 8);
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::crc32;

    #[test]
    fn matches_known_vector() {
        // "123456789" is the standard CRC-32/ISO-HDLC test vector.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn empty_input_is_zero() {
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn different_inputs_differ() {
        assert_ne!(crc32(b"a"), crc32(b"b"));
    }
}
