//! Transport-specific MCP clients. Each transport (stdio, HTTP) owns
//! its connection lifecycle and request/response loop; the shared
//! init / parse helpers and the `MCP_REQUEST_TIMEOUT` ceiling live in
//! [`super::client`].

pub mod http;
pub mod stdio;

pub use http::HttpClient;
pub use stdio::StdioClient;
