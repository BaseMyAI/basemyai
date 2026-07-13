// SPDX-License-Identifier: BUSL-1.1
//! `block_spike` — spike N8.1 (ADR-039 §2, `PLAN-NATIVE-ENGINE.md` §5.1) :
//! prototype **jetable** du format SST par blocs, pour choisir la taille de
//! bloc par mesure, jamais par intuition. Pas un codec de production — les
//! vrais codecs (N8.2) seront écrits contre `format.lock` avec la discipline
//! wire complète ; ici on ne mesure que ce qui départage les tailles :
//!
//! - amplification disque (fichier vs payload), coût index+bloom ;
//! - point lookup froid-process (1 pread par lookup, par construction) ;
//! - efficacité du bloom sur clés absentes (I/O évitées) ;
//! - scan séquentiel ;
//! - coût AEAD par bloc (scellement à l'écriture, descellement par lecture) ;
//! - métadonnées résidentes à l'ouverture (le futur RSS d'ouverture).
//!
//! Matrice : {8, 16, 32, 64 KiB} × {valeurs 100 o "kv", ~1,8 Ko "vecnode"}
//! × {clair, chiffré}. Le profil `vecnode` modèle le vrai enregistrement
//! chaud de BaseMyAI (bloc LM-DiskANN : vecteur 384×f32 + voisins), là où un
//! bloc de 16 KiB ne tient que ~8 entrées.
//!
//! **Limite documentée** : « froid » = froid-process (fichier rouvert, aucun
//! cache applicatif), pas froid-OS — vider le page cache n'est pas portable
//! sous Windows. La comparaison *relative* entre tailles de bloc reste
//! valide (même comportement de cache OS pour les trois), et les compteurs
//! d'octets/IO par lookup sont exacts, indépendants du cache.
//!
//! Rapport archivé : `docs/benchmarks/n8.1-block-size-spike-2026-07-10.md`.
//! Ce binaire peut être supprimé une fois les codecs N8.2+ en place.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

// 8 KiB ajouté au-delà du minimum du plan (16/32/64) pour vérifier que
// 16 KiB est un genou de courbe, pas un artefact de borne de la matrice.
const BLOCK_SIZES: [usize; 4] = [8 * 1024, 16 * 1024, 32 * 1024, 64 * 1024];
const BLOOM_BITS_PER_KEY: usize = 10;
const BLOOM_HASHES: u64 = 7;
const LOOKUPS: usize = 20_000;

fn main() {
    let mut n_small = 500_000u64;
    let mut n_vecnode = 60_000u64;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--n-small" => n_small = num(&mut it, "--n-small"),
            "--n-vecnode" => n_vecnode = num(&mut it, "--n-vecnode"),
            other => {
                eprintln!("unknown arg {other:?} (usage: block_spike [--n-small N] [--n-vecnode N])");
                std::process::exit(1);
            }
        }
    }

    let dir = std::env::temp_dir().join(format!("block-spike-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create spike dir");

    println!(
        "profile,n,block_kib,encrypted,payload_mib,file_mib,amp,index_kib,bloom_kib,blocks,write_s,open_ms,meta_kib,\
         hit_mean_us,hit_p95_us,bytes_per_hit,miss_mean_us,bloom_skip_pct,scan_ms,scan_mibs"
    );
    for (profile, n, val_len) in [("kv", n_small, 100usize), ("vecnode", n_vecnode, 1_800usize)] {
        let entries = dataset(n, val_len);
        let payload: u64 = entries.iter().map(|(k, v)| (k.len() + v.len()) as u64).sum();
        for &block_size in &BLOCK_SIZES {
            for encrypted in [false, true] {
                let row = run_config(&dir, profile, &entries, payload, block_size, encrypted);
                println!("{row}");
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn num(it: &mut impl Iterator<Item = String>, flag: &str) -> u64 {
    it.next()
        .and_then(|v| v.replace('_', "").parse().ok())
        .unwrap_or_else(|| {
            eprintln!("{flag} needs an integer");
            std::process::exit(1);
        })
}

fn run_config(
    dir: &Path,
    profile: &str,
    entries: &[(Vec<u8>, Vec<u8>)],
    payload: u64,
    block_size: usize,
    encrypted: bool,
) -> String {
    let path = dir.join(format!("{profile}-{block_size}-{encrypted}.spike"));
    let cipher = encrypted.then(|| XChaCha20Poly1305::new((&[7u8; 32]).into()));

    let started = Instant::now();
    let written = write_file(&path, entries, block_size, cipher.as_ref());
    let write_s = started.elapsed().as_secs_f64();

    let started = Instant::now();
    let reader = SpikeReader::open(&path, cipher);
    let open_ms = started.elapsed().as_secs_f64() * 1e3;
    let meta_bytes = reader.resident_metadata_bytes();

    // Lookups présents : clés existantes tirées uniformément (déterministe).
    let mut rng = XorShift64::new(0xB10C);
    let mut hit_lat = Vec::with_capacity(LOOKUPS);
    let mut bytes_per_hit = 0u64;
    for _ in 0..LOOKUPS {
        let (key, expected) = {
            let (k, v) = &entries[(rng.next_u64() % entries.len() as u64) as usize];
            (k.clone(), v.clone())
        };
        let t = Instant::now();
        let (found, bytes_read) = reader.lookup(&key);
        hit_lat.push(t.elapsed().as_nanos() as u64);
        bytes_per_hit += bytes_read;
        assert_eq!(
            found.as_deref(),
            Some(expected.as_slice()),
            "lookup must find its value"
        );
    }

    // Lookups absents : le bloom doit éviter l'I/O la plupart du temps.
    let mut miss_lat = Vec::with_capacity(LOOKUPS);
    let mut bloom_skips = 0u64;
    for i in 0..LOOKUPS {
        let key = format!("zz/absent/{i:012}").into_bytes();
        let t = Instant::now();
        let (found, bytes_read) = reader.lookup(&key);
        miss_lat.push(t.elapsed().as_nanos() as u64);
        assert!(found.is_none());
        if bytes_read == 0 {
            bloom_skips += 1;
        }
    }

    let started = Instant::now();
    let scanned = reader.scan_all();
    let scan_ms = started.elapsed().as_secs_f64() * 1e3;
    assert_eq!(scanned, entries.len() as u64, "scan must see every entry");

    hit_lat.sort_unstable();
    format!(
        "{profile},{n},{block_kib},{encrypted},{payload_mib:.1},{file_mib:.1},{amp:.3},{index_kib:.1},{bloom_kib:.1},{blocks},{write_s:.2},{open_ms:.2},{meta_kib:.1},{hit_mean:.1},{hit_p95:.1},{bph},{miss_mean:.1},{skip_pct:.1},{scan_ms:.1},{scan_mibs:.0}",
        n = entries.len(),
        block_kib = block_size / 1024,
        meta_kib = meta_bytes as f64 / 1024.0,
        payload_mib = payload as f64 / (1 << 20) as f64,
        file_mib = written.file_bytes as f64 / (1 << 20) as f64,
        amp = written.file_bytes as f64 / payload as f64,
        index_kib = written.index_bytes as f64 / 1024.0,
        bloom_kib = written.bloom_bytes as f64 / 1024.0,
        blocks = written.n_blocks,
        hit_mean = mean_us(&hit_lat),
        hit_p95 = hit_lat[(hit_lat.len() as f64 * 0.95) as usize] as f64 / 1e3,
        bph = bytes_per_hit / LOOKUPS as u64,
        miss_mean = mean_us(&miss_lat),
        skip_pct = bloom_skips as f64 * 100.0 / LOOKUPS as f64,
        scan_mibs = payload as f64 / (1 << 20) as f64 / (scan_ms / 1e3),
    )
}

fn mean_us(nanos: &[u64]) -> f64 {
    nanos.iter().sum::<u64>() as f64 / nanos.len() as f64 / 1e3
}

// ── écriture (layout ADR-039 §1, version spike) ─────────────────────────────
//
// [data block]* [index] [bloom] [footer fixe: index_off, index_len,
// bloom_off, bloom_len, n_blocks]. En chiffré, chaque section data/index/
// bloom est scellée individuellement, AAD = section_type ‖ section_no.

struct Written {
    file_bytes: u64,
    index_bytes: u64,
    bloom_bytes: u64,
    n_blocks: u64,
}

fn seal(cipher: &XChaCha20Poly1305, plain: &[u8], section: u8, no: u64) -> Vec<u8> {
    // Nonce déterministe pour le spike (section‖no) — les vrais codecs
    // tireront des nonces aléatoires par bloc (ADR-030/039) ; sans effet sur
    // les coûts mesurés ici.
    let mut nonce = [0u8; 24];
    nonce[0] = section;
    nonce[8..16].copy_from_slice(&no.to_be_bytes());
    let aad = [&[section][..], &no.to_be_bytes()].concat();
    let ct = cipher
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plain, aad: &aad })
        .expect("seal");
    [&nonce[..], &(ct.len() as u32).to_be_bytes(), &ct].concat()
}

fn open_sealed(cipher: &XChaCha20Poly1305, sealed: &[u8], section: u8, no: u64) -> Vec<u8> {
    let nonce = &sealed[..24];
    let ct_len = u32::from_be_bytes(sealed[24..28].try_into().expect("ct_len")) as usize;
    let ct = &sealed[28..28 + ct_len];
    let aad = [&[section][..], &no.to_be_bytes()].concat();
    cipher
        .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad: &aad })
        .expect("open sealed section")
}

fn encode_block(entries: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for (k, v) in entries {
        out.extend_from_slice(&(k.len() as u32).to_be_bytes());
        out.extend_from_slice(k);
        out.extend_from_slice(&(v.len() as u32).to_be_bytes());
        out.extend_from_slice(v);
    }
    out
}

fn write_file(
    path: &Path,
    entries: &[(Vec<u8>, Vec<u8>)],
    block_size: usize,
    cipher: Option<&XChaCha20Poly1305>,
) -> Written {
    let mut file = std::fs::File::create(path).expect("create spike file");
    let mut index: Vec<(Vec<u8>, u64, u32)> = Vec::new(); // (last_key, offset, len)
    let mut bloom = Bloom::new(entries.len());
    let mut offset = 0u64;
    let mut block: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut block_payload = 0usize;
    let mut block_no = 0u64;

    let flush_block =
        |block: &mut Vec<(Vec<u8>, Vec<u8>)>, file: &mut std::fs::File, offset: &mut u64, block_no: &mut u64| {
            if block.is_empty() {
                return Vec::new();
            }
            let mut bytes = encode_block(block);
            if let Some(cipher) = cipher {
                bytes = seal(cipher, &bytes, 0, *block_no);
            }
            file.write_all(&bytes).expect("write block");
            let last_key = block.last().expect("non-empty block").0.clone();
            let entry = (last_key, *offset, bytes.len() as u32);
            *offset += bytes.len() as u64;
            *block_no += 1;
            block.clear();
            vec![entry]
        };

    for (k, v) in entries {
        bloom.insert(k);
        block.push((k.clone(), v.clone()));
        block_payload += k.len() + v.len() + 8;
        if block_payload >= block_size {
            index.extend(flush_block(&mut block, &mut file, &mut offset, &mut block_no));
            block_payload = 0;
        }
    }
    index.extend(flush_block(&mut block, &mut file, &mut offset, &mut block_no));

    let mut index_bytes = Vec::new();
    index_bytes.extend_from_slice(&(index.len() as u32).to_be_bytes());
    for (last_key, off, len) in &index {
        index_bytes.extend_from_slice(&(last_key.len() as u32).to_be_bytes());
        index_bytes.extend_from_slice(last_key);
        index_bytes.extend_from_slice(&off.to_be_bytes());
        index_bytes.extend_from_slice(&len.to_be_bytes());
    }
    if let Some(cipher) = cipher {
        index_bytes = seal(cipher, &index_bytes, 1, 0);
    }
    let mut bloom_bytes = bloom.encode();
    if let Some(cipher) = cipher {
        bloom_bytes = seal(cipher, &bloom_bytes, 2, 0);
    }

    let index_off = offset;
    file.write_all(&index_bytes).expect("write index");
    let bloom_off = index_off + index_bytes.len() as u64;
    file.write_all(&bloom_bytes).expect("write bloom");
    let mut footer = Vec::new();
    footer.extend_from_slice(&index_off.to_be_bytes());
    footer.extend_from_slice(&(index_bytes.len() as u64).to_be_bytes());
    footer.extend_from_slice(&bloom_off.to_be_bytes());
    footer.extend_from_slice(&(bloom_bytes.len() as u64).to_be_bytes());
    file.write_all(&footer).expect("write footer");
    file.sync_all().expect("fsync spike file");

    Written {
        file_bytes: bloom_off + bloom_bytes.len() as u64 + footer.len() as u64,
        index_bytes: index_bytes.len() as u64,
        bloom_bytes: bloom_bytes.len() as u64,
        n_blocks: block_no,
    }
}

// ── lecture ──────────────────────────────────────────────────────────────────

struct SpikeReader {
    file: std::cell::RefCell<std::fs::File>,
    index: Vec<(Vec<u8>, u64, u32)>,
    bloom: Bloom,
    cipher: Option<XChaCha20Poly1305>,
    path: PathBuf,
}

impl SpikeReader {
    fn open(path: &Path, cipher: Option<XChaCha20Poly1305>) -> Self {
        let mut file = std::fs::File::open(path).expect("open spike file");
        let file_len = file.metadata().expect("metadata").len();
        file.seek(SeekFrom::Start(file_len - 32)).expect("seek footer");
        let mut footer = [0u8; 32];
        file.read_exact(&mut footer).expect("read footer");
        let index_off = u64::from_be_bytes(footer[0..8].try_into().expect("footer"));
        let index_len = u64::from_be_bytes(footer[8..16].try_into().expect("footer"));
        let bloom_off = u64::from_be_bytes(footer[16..24].try_into().expect("footer"));
        let bloom_len = u64::from_be_bytes(footer[24..32].try_into().expect("footer"));

        let mut index_bytes = vec![0u8; index_len as usize];
        file.seek(SeekFrom::Start(index_off)).expect("seek index");
        file.read_exact(&mut index_bytes).expect("read index");
        let mut bloom_bytes = vec![0u8; bloom_len as usize];
        file.seek(SeekFrom::Start(bloom_off)).expect("seek bloom");
        file.read_exact(&mut bloom_bytes).expect("read bloom");
        if let Some(cipher) = &cipher {
            index_bytes = open_sealed(cipher, &index_bytes, 1, 0);
            bloom_bytes = open_sealed(cipher, &bloom_bytes, 2, 0);
        }

        let mut index = Vec::new();
        let n = u32::from_be_bytes(index_bytes[0..4].try_into().expect("index n")) as usize;
        let mut at = 4usize;
        for _ in 0..n {
            let klen = u32::from_be_bytes(index_bytes[at..at + 4].try_into().expect("klen")) as usize;
            at += 4;
            let key = index_bytes[at..at + klen].to_vec();
            at += klen;
            let off = u64::from_be_bytes(index_bytes[at..at + 8].try_into().expect("off"));
            at += 8;
            let len = u32::from_be_bytes(index_bytes[at..at + 4].try_into().expect("len"));
            at += 4;
            index.push((key, off, len));
        }

        Self {
            file: std::cell::RefCell::new(std::fs::File::open(path).expect("reopen")),
            index,
            bloom: Bloom::decode(&bloom_bytes),
            cipher,
            path: path.to_path_buf(),
        }
    }

    fn resident_metadata_bytes(&self) -> u64 {
        let index: usize = self.index.iter().map(|(k, _, _)| k.len() + 12 + 24).sum();
        (index + self.bloom.bits.len()) as u64
    }

    /// Retourne (valeur trouvée, octets lus sur disque pour ce lookup).
    fn lookup(&self, key: &[u8]) -> (Option<Vec<u8>>, u64) {
        if !self.bloom.contains(key) {
            return (None, 0);
        }
        let block_idx = self.index.partition_point(|(last, _, _)| last.as_slice() < key);
        let Some((_, off, len)) = self.index.get(block_idx) else {
            return (None, 0);
        };
        let (block_no, bytes) = (block_idx as u64, self.read_span(*off, *len));
        let read = bytes.len() as u64;
        let plain = match &self.cipher {
            Some(cipher) => open_sealed(cipher, &bytes, 0, block_no),
            None => bytes,
        };
        (search_block(&plain, key), read)
    }

    fn scan_all(&self) -> u64 {
        let mut total = 0u64;
        for (i, (_, off, len)) in self.index.iter().enumerate() {
            let bytes = self.read_span(*off, *len);
            let plain = match &self.cipher {
                Some(cipher) => open_sealed(cipher, &bytes, 0, i as u64),
                None => bytes,
            };
            total += u32::from_be_bytes(plain[0..4].try_into().expect("count")) as u64;
        }
        total
    }

    fn read_span(&self, off: u64, len: u32) -> Vec<u8> {
        let mut file = self.file.borrow_mut();
        file.seek(SeekFrom::Start(off))
            .unwrap_or_else(|e| panic!("seek {}: {e}", self.path.display()));
        let mut buf = vec![0u8; len as usize];
        file.read_exact(&mut buf)
            .unwrap_or_else(|e| panic!("read {}: {e}", self.path.display()));
        buf
    }
}

fn search_block(plain: &[u8], key: &[u8]) -> Option<Vec<u8>> {
    let n = u32::from_be_bytes(plain[0..4].try_into().expect("count")) as usize;
    let mut at = 4usize;
    // Parcours linéaire du bloc : les entrées sont length-prefixées, une
    // recherche binaire exigerait des restart points (hors périmètre spike ;
    // le coût linéaire intra-bloc fait partie de ce qu'on mesure).
    for _ in 0..n {
        let klen = u32::from_be_bytes(plain[at..at + 4].try_into().expect("klen")) as usize;
        at += 4;
        let k = &plain[at..at + klen];
        at += klen;
        let vlen = u32::from_be_bytes(plain[at..at + 4].try_into().expect("vlen")) as usize;
        at += 4;
        if k == key {
            return Some(plain[at..at + vlen].to_vec());
        }
        if k > key {
            return None; // trié : dépassé
        }
        at += vlen;
    }
    None
}

// ── bloom (double hashing h1 + i·h2, façon ADR-039 §6) ──────────────────────

struct Bloom {
    bits: Vec<u8>,
}

impl Bloom {
    fn new(n_keys: usize) -> Self {
        let n_bits = (n_keys * BLOOM_BITS_PER_KEY).next_power_of_two();
        Self {
            bits: vec![0u8; n_bits / 8],
        }
    }

    fn hashes(&self, key: &[u8]) -> (u64, u64) {
        let mut h1 = DefaultHasher::new();
        key.hash(&mut h1);
        let mut h2 = DefaultHasher::new();
        0xB10Cu64.hash(&mut h2);
        key.hash(&mut h2);
        (h1.finish(), h2.finish() | 1)
    }

    fn insert(&mut self, key: &[u8]) {
        let (h1, h2) = self.hashes(key);
        let n_bits = (self.bits.len() * 8) as u64;
        for i in 0..BLOOM_HASHES {
            let bit = h1.wrapping_add(i.wrapping_mul(h2)) % n_bits;
            self.bits[(bit / 8) as usize] |= 1 << (bit % 8);
        }
    }

    fn contains(&self, key: &[u8]) -> bool {
        let (h1, h2) = self.hashes(key);
        let n_bits = (self.bits.len() * 8) as u64;
        (0..BLOOM_HASHES).all(|i| {
            let bit = h1.wrapping_add(i.wrapping_mul(h2)) % n_bits;
            self.bits[(bit / 8) as usize] & (1 << (bit % 8)) != 0
        })
    }

    fn encode(&self) -> Vec<u8> {
        self.bits.clone()
    }

    fn decode(bytes: &[u8]) -> Self {
        Self { bits: bytes.to_vec() }
    }
}

// ── data ─────────────────────────────────────────────────────────────────────

fn dataset(n: u64, val_len: usize) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut rng = XorShift64::new(42);
    (0..n)
        .map(|i| {
            let key = format!("kv/{:06}/{:06}", i / 1_000, i % 1_000).into_bytes();
            let value: Vec<u8> = (0..val_len).map(|_| (rng.next_u64() & 0xFF) as u8).collect();
            (key, value)
        })
        .collect()
}

/// Duplicata documenté (contrainte `src/bin`, comme `engine_bench`).
struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}
