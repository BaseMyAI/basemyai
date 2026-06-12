//! Types de la recherche vectorielle. Les opérations (`vector_upsert`,
//! `vector_knn`) sont **natives** sur [`Store`](crate::Store) via libSQL — pas
//! de trait/backend à abstraire (ADR : pivot libSQL).
//!
//! **Le pattern clé** subsiste : `vector_knn` accepte un [`Filter`] *fourni par
//! l'appelant*. Le core applique le filtre sans en connaître le sens. Le filtre
//! est **paramétré** — fragment SQL `?` + valeurs liées — donc agnostique *et*
//! anti-injection.

/// Valeur SQL liée à un placeholder `?` d'un [`Filter`].
///
/// `#[non_exhaustive]` : de nouveaux types SQL libSQL peuvent être ajoutés.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Value {
    /// Entier signé 64 bits.
    Integer(i64),
    /// Flottant 64 bits.
    Real(f64),
    /// Texte UTF-8.
    Text(String),
    /// Données binaires brutes.
    Blob(Vec<u8>),
    /// Valeur SQL NULL.
    Null,
}

/// Filtre SQL paramétré fourni par le consommateur. `where_sql` contient des
/// `?` ; les valeurs (potentiellement non fiables) vivent dans `params`.
#[derive(Debug, Default, Clone)]
pub struct Filter {
    /// Fragment `WHERE` avec des `?` anonymes (anti-injection).
    pub where_sql: String,
    /// Valeurs liées aux `?`, dans l'ordre textuel.
    pub params: Vec<Value>,
}

impl Filter {
    /// Construit un filtre à partir d'un fragment `WHERE` et de ses paramètres.
    #[must_use]
    pub fn new(where_sql: impl Into<String>, params: Vec<Value>) -> Self {
        Self {
            where_sql: where_sql.into(),
            params,
        }
    }
}

/// Un voisin retourné par `vector_knn`.
#[derive(Debug, Clone)]
pub struct Neighbor {
    /// Identifiant de la ligne (`id TEXT PRIMARY KEY`).
    pub id: String,
    /// Distance cosinus réelle dans `[0, 2]` (`0` = identique).
    pub distance: f32,
}
