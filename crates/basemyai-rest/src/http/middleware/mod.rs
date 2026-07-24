// SPDX-License-Identifier: BUSL-1.1
//! Middlewares transverses, assemblés par `server::router`. Chacun a une
//! seule responsabilité ; aucun n'est ajouté "au cas où".

pub mod auth;
pub mod body_limit;
pub mod request_id;
pub mod timeout;
pub mod tracing;
