//! Tests d'intégration de la Reciprocal Rank Fusion (`rrf_fuse`).
//!
//! Vérifie la sémantique RRF : score `Σ 1/(k+rang)`, traçabilité des signaux
//! contributeurs, tri décroissant à départage déterministe, et cas limites.

use basemyai::{Fused, Ranking, RRF_K, rrf_fuse};

/// Petit utilitaire : construit un `Ranking` à partir de littéraux.
fn ranking(signal: &str, ids: &[&str]) -> Ranking {
    Ranking {
        signal: signal.to_string(),
        ids: ids.iter().map(|s| (*s).to_string()).collect(),
    }
}

/// Tolérance pour les comparaisons de scores flottants.
const EPS: f64 = 1e-12;

/// Récupère l'entrée fusionnée d'un id donné (panique en test si absente).
fn find<'a>(fused: &'a [Fused], id: &str) -> &'a Fused {
    fused
        .iter()
        .find(|f| f.id == id)
        .expect("l'id attendu doit être présent dans le résultat fusionné")
}

#[test]
fn rrf_favorise_les_ids_presents_dans_plusieurs_signaux() {
    // Effet caractéristique de la RRF : un id de consensus (présent dans
    // plusieurs signaux, même sans jamais être n°1) bat un id n°1 d'un seul
    // signal. "x" est n°1 de `vector` mais absent de `graph` ; "y" est 2e des
    // deux signaux. Le cumul de "y" doit dépasser le pic isolé de "x".
    let rankings = [
        ranking("vector", &["x", "y"]),
        ranking("graph", &["z", "y"]),
    ];

    let fused = rrf_fuse(&rankings, RRF_K);

    let x = find(&fused, "x").score; // 1/60
    let y = find(&fused, "y").score; // 1/61 + 1/61
    assert!(y > x, "y (consensus 2 signaux) doit battre x (pic isolé)");
    assert_eq!(fused[0].id, "y", "y doit être premier");
}

#[test]
fn contributions_liste_les_signaux_sans_doublon_dans_l_ordre_de_premiere_apparition() {
    // "m" apparaît dans vector, puis recency, puis de nouveau dans graph.
    let rankings = [
        ranking("vector", &["m", "n"]),
        ranking("recency", &["m"]),
        ranking("graph", &["n", "m"]),
    ];

    let fused = rrf_fuse(&rankings, RRF_K);

    let m = find(&fused, "m");
    // Ordre de première apparition du signal : vector, recency, graph.
    assert_eq!(
        m.contributions,
        vec![
            "vector".to_string(),
            "recency".to_string(),
            "graph".to_string()
        ],
        "contributions ordonnées par première apparition, sans doublon"
    );

    let n = find(&fused, "n");
    assert_eq!(
        n.contributions,
        vec!["vector".to_string(), "graph".to_string()],
    );
}

#[test]
fn score_exact_sur_un_petit_cas_connu() {
    // id "a" en rang 0 de deux signaux avec k=60 → score = 2/60.
    let k = 60.0;
    let rankings = [
        ranking("s1", &["a", "b"]),
        ranking("s2", &["a", "c"]),
    ];

    let fused = rrf_fuse(&rankings, k);

    let a = find(&fused, "a");
    assert!(
        (a.score - 2.0 / 60.0).abs() < EPS,
        "a doit valoir 2/60, obtenu {}",
        a.score
    );

    // b : rang 1 de s1 → 1/61. c : rang 1 de s2 → 1/61.
    let b = find(&fused, "b");
    assert!((b.score - 1.0 / 61.0).abs() < EPS, "b doit valoir 1/61");
    let c = find(&fused, "c");
    assert!((c.score - 1.0 / 61.0).abs() < EPS, "c doit valoir 1/61");

    // a en tête.
    assert_eq!(fused[0].id, "a");
}

#[test]
fn departage_deterministe_par_id_croissant_a_score_egal() {
    // Trois ids tous en rang 0 d'un signal distinct → score identique 1/60.
    // Le départage doit les trier par id croissant : "alpha", "beta", "gamma".
    let rankings = [
        ranking("s1", &["gamma"]),
        ranking("s2", &["alpha"]),
        ranking("s3", &["beta"]),
    ];

    let fused = rrf_fuse(&rankings, 60.0);

    let ids: Vec<&str> = fused.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["alpha", "beta", "gamma"],
        "à score égal, tri par id croissant (lexicographique)"
    );

    // Tous les scores sont strictement égaux.
    for f in &fused {
        assert!((f.score - 1.0 / 60.0).abs() < EPS);
    }
}

#[test]
fn cas_limite_rankings_globalement_vide() {
    let fused = rrf_fuse(&[], RRF_K);
    assert!(fused.is_empty(), "entrée vide → résultat vide");
}

#[test]
fn cas_limite_ranking_aux_ids_vides_est_ignore() {
    // Un classement vide ne doit ni planter ni introduire d'entrée parasite.
    let rankings = [
        ranking("vide", &[]),
        ranking("plein", &["a"]),
        ranking("vide2", &[]),
    ];

    let fused = rrf_fuse(&rankings, RRF_K);

    assert_eq!(fused.len(), 1, "seul l'id réel doit apparaître");
    let a = find(&fused, "a");
    assert!((a.score - 1.0 / 60.0).abs() < EPS);
    // Aucun signal vide ne doit figurer dans les contributions.
    assert_eq!(a.contributions, vec!["plein".to_string()]);
}
