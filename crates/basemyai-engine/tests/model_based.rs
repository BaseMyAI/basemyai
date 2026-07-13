// SPDX-License-Identifier: BUSL-1.1
//! Tests model-based (N11.2, `docs/PLAN-NATIVE-ENGINE.md` §8.1) : un
//! `BTreeMap<Vec<u8>, Vec<u8>>` sert de modèle de référence, et une séquence
//! d'opérations pondérées, dérivée d'un PRNG xorshift64* déterministe (même
//! discipline que `src/harness.rs` : hash multiplicatif du seed, pas de
//! dépendance `rand`), est rejouée contre le moteur réel en comparant l'état
//! après chaque étape mutante.
//!
//! Gate rapide à chaque PR (§8.3) : bornée en opérations/seeds pour tourner
//! en quelques secondes, pas la campagne nightly longue (fuzzing par codec,
//! workloads 100k, kill loops prolongés — ailleurs). `EngineOptions` petit
//! (`small_options`, même esprit que `tests/corruption_smoke.rs`/
//! `tests/failpoints.rs`) force flush/compaction à déclencher réellement sur
//! une séquence bornée.
//!
//! Chaque propriété du §8.1 est testée par une assertion directement
//! attachée à l'opération qui l'exerce (pas seulement "ça ne panique pas") :
//!
//! - **last-write-wins** : après chaque `put`, `get(key)` doit retourner
//!   exactement la valeur qui vient d'être écrite.
//! - **batch présent-en-entier-ou-absent** : après chaque `apply_batch`
//!   réussi, CHAQUE op stagée est revérifiée individuellement — un commit
//!   partiel se verrait comme un `get` divergent sur au moins une clé du
//!   lot. Le versant "panne pendant l'écriture du batch" est hors périmètre
//!   ici (déjà couvert par `tests/failpoints.rs` + `tests/crash_consistency.rs`
//!   — ce fichier n'utilise volontairement aucun failpoint, §8.1/§8.3).
//! - **suppression persistante** : `get` retourne `None` immédiatement après
//!   `delete`, et le reste via toutes les comparaisons modèle ultérieures
//!   (reopen/crash/compact n'y changent rien, puisque le modèle n'a plus la
//!   clé non plus).
//! - **scan ordonné** : `scan_prefix(b"")` (préfixe vide = tout le store) est
//!   comparé élément par élément à `model.iter()` (déjà trié) — un ordre
//!   divergent fait échouer l'égalité de vecteurs, pas seulement les
//!   clés/valeurs prises isolément.
//! - **réouverture identique** : comparaison complète après `close()` +
//!   réouverture (gracieuse) ET après `drop` sans `close()` suivi d'une
//!   réouverture (arrêt sale — exerce le vrai replay WAL ; jamais de perte
//!   ici puisque chaque `put`/`delete`/`apply_batch` fsync avant de
//!   retourner `Ok` — une vraie perte mid-write est le rôle de
//!   `tests/crash_consistency.rs`, pas de ce fichier).
//! - **aucun record ressuscité après compaction** : toute clé un jour
//!   supprimée (`ever_deleted`) est revérifiée contre le modèle juste après
//!   chaque `compact_now` — une résurrection depuis une couche SST non
//!   fusionnée se verrait immédiatement.
//!
//! Clair et chiffré (`Engine::open_encrypted_with_options`), conformément à
//! la convention du repo (`tests/failpoints.rs`, `tests/corruption_smoke.rs`).

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use basemyai_engine::{Batch, Engine, EngineOptions};

/// Même tuning que `tests/corruption_smoke.rs`/`tests/failpoints.rs` : petit
/// pour que flush/compaction se déclenchent réellement sur une séquence
/// courte, pas seulement une fois le store déjà gros.
fn small_options() -> EngineOptions {
    EngineOptions {
        memtable_flush_threshold: 5,
        compaction_sst_threshold: 3,
        block_size: 256,
        ..EngineOptions::default()
    }
}

const KEY_SPACE: usize = 20;

fn key_for(id: usize) -> Vec<u8> {
    format!("model-key-{id:03}").into_bytes()
}

/// xorshift64* déterministe, seedé par un hash multiplicatif du seed d'entrée
/// — la même construction que `src/harness.rs` utilise partout (aucune
/// dépendance `rand` dans ce workspace).
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).max(1))
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn next_index(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }

    fn next_bytes(&mut self, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            out.extend_from_slice(&self.next_u64().to_le_bytes());
        }
        out.truncate(len);
        out
    }

    /// Un payload de 1..=32 octets — jamais vide, pour rester visuellement
    /// distinct d'une tombstone (`Option::None`) dans la sortie d'échec.
    fn next_value(&mut self) -> Vec<u8> {
        let len = 1 + self.next_index(32);
        self.next_bytes(len)
    }

    /// `true` avec probabilité `numerator / denominator`.
    fn next_chance(&mut self, numerator: u64, denominator: u64) -> bool {
        self.next_u64() % denominator < numerator
    }
}

#[derive(Debug, Clone, Copy)]
enum Op {
    Put,
    Get,
    Delete,
    Batch,
    Flush,
    Compact,
    Reopen,
    Crash,
    PrefixScan,
    RotateKey,
}

/// Sélection pondérée : `Put`/`Get`/`Delete` dominent (chemin quotidien),
/// `Batch`/`PrefixScan` assez fréquents pour être exercés à répétition, les
/// ops de cycle de vie du store (`Flush`/`Compact`/`Reopen`/`Crash`/
/// `RotateKey`) rares — chacune coûte relativement cher (vraie I/O, un
/// `Engine::open` complet) et le brief de ce fichier (§8.3) est un gate de PR
/// *court*, pas la campagne nightly. `RotateKey` n'est jamais proposé en
/// clair (rien à tourner).
fn pick_op(rng: &mut Rng, encrypted: bool) -> Op {
    let mut table: Vec<(Op, u32)> = vec![
        (Op::Put, 28),
        (Op::Get, 20),
        (Op::Delete, 14),
        (Op::Batch, 10),
        (Op::PrefixScan, 8),
        (Op::Flush, 6),
        (Op::Compact, 5),
        (Op::Reopen, 4),
        (Op::Crash, 4),
    ];
    if encrypted {
        table.push((Op::RotateKey, 3));
    }
    let total: u32 = table.iter().map(|(_, w)| *w).sum();
    let mut roll = (rng.next_u64() % u64::from(total)) as u32;
    for (op, weight) in table {
        if roll < weight {
            return op;
        }
        roll -= weight;
    }
    unreachable!("weights sum to `total`, so `roll < total` always hits an arm before this point")
}

/// Possède le répertoire du store et l'`Engine` actuellement ouvert, pour que
/// `Reopen`/`Crash` puissent le consommer-et-remplacer (`Engine::close` prend
/// `self`, et un `drop` propre a besoin du même slot libre pour rouvrir).
struct Harness {
    _dir: tempfile::TempDir,
    dir_path: PathBuf,
    encrypted: bool,
    key: Vec<u8>,
    options: EngineOptions,
    engine: Option<Engine>,
}

impl Harness {
    fn new(encrypted: bool) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir_path = dir.path().to_path_buf();
        let mut harness = Self {
            _dir: dir,
            dir_path,
            encrypted,
            key: b"model-based-harness-initial-key".to_vec(),
            options: small_options(),
            engine: None,
        };
        harness.open();
        harness
    }

    fn open(&mut self) {
        let opened = if self.encrypted {
            Engine::open_encrypted_with_options(&self.dir_path, &self.key, self.options).expect("open encrypted")
        } else {
            Engine::open_with_options(&self.dir_path, self.options).expect("open clear")
        };
        self.engine = Some(opened);
    }

    fn engine(&self) -> &Engine {
        self.engine
            .as_ref()
            .expect("harness invariant: engine present between steps")
    }

    fn engine_mut(&mut self) -> &mut Engine {
        self.engine
            .as_mut()
            .expect("harness invariant: engine present between steps")
    }

    /// Réouverture gracieuse : `close()` flush le memtable, donc ceci exerce
    /// des lectures sur SST fraîchement écrites, pas le replay WAL.
    fn reopen_gracefully(&mut self) {
        let engine = self
            .engine
            .take()
            .expect("harness invariant: engine present before reopen");
        engine.close().expect("close");
        self.open();
    }

    /// Arrêt sale : `drop` sans `close()`, puis réouverture — exerce le vrai
    /// chemin de replay WAL (`Engine::open_inner`'s `wal.replay()`). Ne perd
    /// jamais rien ici : chaque `put`/`delete`/`apply_batch` a déjà fsync
    /// avant de retourner `Ok`, donc il n'y a rien "en vol" à perdre sans un
    /// vrai kill de process (le rôle de `tests/crash_consistency.rs`, pas de
    /// ce fichier — §8.1's brief pour les tests model-based).
    fn crash(&mut self) {
        let engine = self
            .engine
            .take()
            .expect("harness invariant: engine present before crash");
        drop(engine);
        self.open();
    }

    fn rotate_key(&mut self, new_key: Vec<u8>) {
        self.engine_mut().rotate_key(&new_key).expect("rotate_key");
        self.key = new_key;
    }
}

/// Compare tout le keyspace vivant du moteur au `model`, élément par élément
/// et dans l'ordre — `scan_prefix(b"")` (préfixe vide = toutes les clés) est
/// la lecture complète du store ; `model.iter()` est déjà ascendant par
/// ordre `Vec<u8>`, le même ordre que `Key` (`key::Key` : lexicographique sur
/// les octets bruts). Un ordre divergent, pas seulement un contenu
/// divergent, fait échouer cette assertion (§8.1 "scan ordonné").
fn assert_matches_model(engine: &Engine, model: &BTreeMap<Vec<u8>, Vec<u8>>, context: &str) {
    let scanned = engine
        .scan_prefix(b"")
        .unwrap_or_else(|e| panic!("{context}: scan_prefix(b\"\") failed: {e}"));
    let observed: Vec<(Vec<u8>, Vec<u8>)> = scanned
        .iter()
        .map(|(k, v)| (k.as_bytes().to_vec(), v.clone()))
        .collect();
    let expected: Vec<(Vec<u8>, Vec<u8>)> = model.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    assert_eq!(
        observed, expected,
        "{context}: full-store scan diverges from the BTreeMap reference model"
    );
}

fn run_sequence(seed: u64, encrypted: bool, op_count: usize) {
    let mut rng = Rng::new(seed);
    let mut harness = Harness::new(encrypted);
    let mut model: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    let mut ever_deleted: HashSet<Vec<u8>> = HashSet::new();

    assert_matches_model(
        harness.engine(),
        &model,
        &format!("seed {seed} encrypted={encrypted}: empty store"),
    );

    for step in 0..op_count {
        let ctx = |suffix: &str| format!("seed {seed} encrypted={encrypted} step {step}: {suffix}");
        match pick_op(&mut rng, encrypted) {
            Op::Put => {
                let key = key_for(rng.next_index(KEY_SPACE));
                let value = rng.next_value();
                harness.engine_mut().put(&key, &value).expect("put");
                model.insert(key.clone(), value.clone());
                // Last-write-wins : la valeur qui vient d'être écrite doit
                // être exactement ce qu'un `get` frais retourne, jamais une
                // valeur antérieure figée.
                assert_eq!(
                    harness.engine().get(&key).expect("get"),
                    Some(value),
                    "{}",
                    ctx("last-write-wins violated after put")
                );
            }
            Op::Get => {
                let key = key_for(rng.next_index(KEY_SPACE));
                assert_eq!(
                    harness.engine().get(&key).expect("get"),
                    model.get(&key).cloned(),
                    "{}",
                    ctx("get diverges from model")
                );
            }
            Op::Delete => {
                let key = key_for(rng.next_index(KEY_SPACE));
                harness.engine_mut().delete(&key).expect("delete");
                model.remove(&key);
                ever_deleted.insert(key.clone());
                assert_eq!(
                    harness.engine().get(&key).expect("get"),
                    None,
                    "{}",
                    ctx("delete must be immediately visible")
                );
            }
            Op::Batch => {
                let mut batch = Batch::new();
                let mut staged: Vec<(Vec<u8>, Option<Vec<u8>>)> = Vec::new();
                for _ in 0..2 + rng.next_index(3) {
                    let key = key_for(rng.next_index(KEY_SPACE));
                    if rng.next_chance(7, 10) {
                        let value = rng.next_value();
                        batch.put(&key, &value);
                        staged.push((key, Some(value)));
                    } else {
                        batch.delete(&key);
                        staged.push((key, None));
                    }
                }
                harness.engine_mut().apply_batch(&batch).expect("apply_batch");
                // La mise à jour du modèle reflète la sémantique documentée
                // de `Batch` elle-même : dans un même batch, une op plus
                // tardive sur la même clé gagne sur une plus ancienne
                // (`Batch::put`'s doc) — un `insert`/`remove` séquentiel sur
                // `staged` reproduit exactement ça.
                for (key, value) in &staged {
                    match value {
                        Some(v) => {
                            model.insert(key.clone(), v.clone());
                        }
                        None => {
                            model.remove(key);
                            ever_deleted.insert(key.clone());
                        }
                    }
                }
                // Batch présent-en-entier-ou-absent (versant succès — le
                // versant "panne pendant l'écriture" est le rôle de
                // `tests/failpoints.rs`/`tests/crash_consistency.rs`, pas de
                // ce fichier) : l'effet final (post-dédup) de CHAQUE op
                // stagée doit être visible maintenant. Un commit partiel
                // divergerait sur au moins une clé.
                for (key, _) in &staged {
                    assert_eq!(
                        harness.engine().get(key).expect("get"),
                        model.get(key).cloned(),
                        "{}",
                        ctx(&format!("batch op for key {key:?} not fully applied"))
                    );
                }
            }
            Op::Flush => {
                harness.engine_mut().flush().expect("flush");
                assert_matches_model(harness.engine(), &model, &ctx("after flush"));
            }
            Op::Compact => {
                harness.engine_mut().compact_now().expect("compact_now");
                // Aucun record ressuscité après compaction : toute clé un
                // jour supprimée doit encore correspondre à la vue actuelle
                // du modèle (`None`, sauf réinsertion depuis) — une valeur
                // périmée survivant dans une couche non fusionnée
                // apparaîtrait immédiatement ici.
                for key in &ever_deleted {
                    assert_eq!(
                        harness.engine().get(key).expect("get"),
                        model.get(key).cloned(),
                        "{}",
                        ctx(&format!("key {key:?} resurrected by compaction"))
                    );
                }
                assert_matches_model(harness.engine(), &model, &ctx("after compact_now"));
            }
            Op::Reopen => {
                harness.reopen_gracefully();
                assert_matches_model(harness.engine(), &model, &ctx("after graceful reopen"));
            }
            Op::Crash => {
                harness.crash();
                assert_matches_model(harness.engine(), &model, &ctx("after unclean drop + reopen"));
            }
            Op::PrefixScan => {
                assert_matches_model(harness.engine(), &model, &ctx("scan_prefix(\"\") checkpoint"));
            }
            Op::RotateKey => {
                let new_key = rng.next_bytes(24);
                harness.rotate_key(new_key);
                // La rotation re-scelle seulement le DEK (ADR-030 §4) : le
                // contenu ne doit pas bouger d'un octet.
                assert_matches_model(harness.engine(), &model, &ctx("after rotate_key"));
            }
        }
    }

    assert_matches_model(
        harness.engine(),
        &model,
        &format!("seed {seed} encrypted={encrypted}: end of sequence"),
    );
    // Réouverture gracieuse finale : la forme la plus forte de "réouverture
    // identique" — quelle que soit la dernière opération, fermer puis
    // rouvrir depuis zéro doit reproduire le modèle exactement.
    harness.reopen_gracefully();
    assert_matches_model(
        harness.engine(),
        &model,
        &format!("seed {seed} encrypted={encrypted}: final reopen"),
    );
}

/// Seeds en clair — plusieurs runs courts indépendants plutôt qu'un seul
/// long : des seeds distincts exercent des ordres d'opérations/collisions de
/// clés différents sans avoir besoin d'un `op_count` plus gros par run (garde
/// ce fichier rapide, gate de PR §8.3).
const CLEAR_SEEDS: &[u64] = &[1, 2, 3, 4, 5, 6, 7];
const ENCRYPTED_SEEDS: &[u64] = &[101, 102, 103];

#[test]
fn model_based_clear_store_matches_btreemap_reference() {
    for &seed in CLEAR_SEEDS {
        run_sequence(seed, false, 140);
    }
}

#[test]
fn model_based_encrypted_store_matches_btreemap_reference() {
    for &seed in ENCRYPTED_SEEDS {
        run_sequence(seed, true, 100);
    }
}
