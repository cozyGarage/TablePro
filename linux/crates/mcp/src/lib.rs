//! MCP server surface for TablePro Linux.
//!
//! Every tool call obtains a connection through a [`ConnectionProvider`]
//! that must return a policy-gated handle. There is no path to a raw
//! driver connection from this crate.

mod auth;
mod bridge;
mod rate_limit;
mod server;
mod tokens;
mod tools;

pub use auth::{McpScope, TokenPermissions, authorize_scopes};
pub use bridge::{ConnectionProvider, McpBridge};
pub use rate_limit::RateLimiter;
pub use server::{McpServerConfig, serve_stdio, serve_streamable_http};
pub use tokens::{McpToken, TokenStore, generate_token};
pub use tools::TOOL_NAMES;
