# N12 — Baseline Argon2id (2026-07-15)

## Objet

PR2 d'ADR-042 mesure le profil validé pour les passphrases, avant qu'il soit
branché au format `CryptoMeta:2` : Argon2id, `m=65536 KiB` (64 MiB), `t=3`,
`p=4`, sortie de 32 octets. Ce n'est pas un benchmark du moteur et aucun
store n'a été créé ou modifié par cette mesure.

## Commande

```text
cargo run --release -p basemyai-engine --features test-util --bin argon2_bench -- 5
```

Le binaire exécute cinq dérivations indépendantes avec un salt fixe de test,
un buffer de sortie `Zeroizing<[u8; 32]>`, puis publie moyenne, médiane et
p95. La passphrase est une constante de benchmark sans valeur de production.

## Environnement

- Windows 11 Famille 10.0.26200
- Intel Core i7-13620H
- 13.7 GiB RAM visible par l'OS
- Build `release`, RustCrypto `argon2` 0.5.3 avec la feature `zeroize`

## Résultat

```json
{"algorithm":"argon2id","m_kib":65536,"t_cost":3,"p":4,"iterations":5,"mean_ms":122.43108,"median_ms":123.1868,"p95_ms":128.0729}
```

Le profil reste sous la cible ADR (« quelques centaines de ms ») sur cette
machine. Cette mesure unique n'est pas une promesse pour les machines à faible
mémoire ou les environnements virtualisés ; PR3 devra conserver ce benchmark
et remesurer si les paramètres par défaut changent.
