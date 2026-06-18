//! Transports MCP : `stdio` (intégration agent local) et `http` (Streamable HTTP
//! avec auth Bearer). Chacun derrière sa feature.

#[cfg(feature = "http")]
mod http;
#[cfg(feature = "stdio")]
mod stdio;

#[cfg(feature = "http")]
pub use http::run_http;
#[cfg(feature = "stdio")]
pub use stdio::run_stdio;
