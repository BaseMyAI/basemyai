//! Types de la recherche vectorielle. Les opérations (`vector_upsert`,
//! `vector_knn`) sont **natives** sur [`Store`](crate::Store) via libSQL — pas
//! de trait/backend à abstraire (ADR : pivot libSQL).
//!
//! **Le pattern clé** subsiste : `vector_knn` accepte un [`Filter`] *fourni par
//! l'appelant*. Le core applique le filtre sans en connaître le sens. Le filtre
//! est **paramétré** — fragment SQL `?` + valeurs liées — donc agnostique *et*
//! anti-injection.

/// Valeur SQL liée à un placeholder `?` d'un [`Filter`].
#[derive(Debug, Clone)]
pub enum Value {
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
    Null,
}

/// Filtre SQL paramétré fourni par le consommateur. `where_sql` contient des
/// `?` ; les valeurs (potentiellement non fiables) vivent dans `params`.
#[derive(Debug, Default, Clone)]
pub struct Filter {
    pub where_sql: String,
    pub params: Vec<Value>,
}

impl Filter {
    /// Construit un filtre à partir d'un fragment `WHERE` et de ses paramètres.
    #[must_use]
    pub fn new(where_sql: impl Into<String>, params: Vec<Value>) -> Self {
        Self { where_sql: where_sql.into(), params }
    }
}

/// Un voisin retourné par `vector_knn`.
#[derive(Debug, Clone)]
pub struct Neighbor {
    pub id: String,
    pub distance: f32,
}
