# ADR-007 — Chiffrement au repos — sqlcipher

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

Les données de mémoire (conversations, profils, faits sur les utilisateurs) sont parmi les plus sensibles d'un produit IA. Au repos, le fichier `.db` ne doit pas être lisible par quiconque accède au disque. Pour les personas sous contrainte compliance (santé, finance), le chiffrement au repos n'est pas optionnel.

**Décision**

Chiffrement via **sqlcipher** (fork de SQLite chiffrant pages et journal). La DB s'ouvre avec une `encryption_key` ; le fichier sur disque est illisible sans elle. La clé est **fournie à l'ouverture, jamais stockée**.

Statut différencié par niveau :
- Dans **`basemyai-core`** : sqlcipher est **optionnel**. `Store::open(path, key: Option<EncryptionKey>)`.
- Dans **`basemyai`** : le chiffrement est **obligatoire**. Instancier une mémoire sans `encryption_key` échoue.
- (Côté ForgeMyAI, consommateur du core : chiffrement **off par défaut** — un index de code est moins sensible, et le coût perf n'est pas justifié. Décision propre à ForgeMyAI.)

**Conséquences**

✅ Fichier mémoire illisible hors du process sans la clé.
✅ Argument compliance direct.
✅ Le core garde le chiffrement optionnel → réutilisable par des consommateurs qui n'en veulent pas (ForgeMyAI).
⚠️ **Compatibilité de build sqlcipher + sqlite-vec à valider** : sqlcipher est un fork de SQLite ; le linkage de l'extension n'est pas garanti. **Risque accepté, pas de spike préalable.** Repli de provisioning si le linkage échoue : build SQLite custom, ou chargement dynamique de l'extension dans le build sqlcipher.
⚠️ Gestion de la clé déléguée au consommateur : si la clé est perdue, les données sont irrécupérables (par conception).
⚠️ Léger surcoût de perf (chiffrement/déchiffrement des pages).

**Alternatives rejetées**

Chiffrement applicatif champ-par-champ — casse la recherche vectorielle (on ne peut pas faire de KNN sur des vecteurs chiffrés), complexe et partiel.

Chiffrement du système de fichiers (LUKS, BitLocker) — hors du contrôle du produit, pas portable, ne protège pas une fois le volume monté.

Pas de chiffrement — inacceptable pour les personas compliance ; rejeté pour `basemyai`.
