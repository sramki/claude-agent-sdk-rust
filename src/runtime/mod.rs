//! The live runtime: driving the `claude` CLI over the stream-json protocol.
//!
//! Ported from the Python `_internal/` runtime (`message_parser`, `transport`,
//! `query`) plus the public `query` / `client` entry points. Built
//! incrementally; this module currently exposes the message parser.

pub mod api;
pub mod client;
pub mod message_parser;
pub mod query;
pub mod transport;

pub use api::{query, query_with_transport, MessageStream, Prompt};
pub use client::Client;
pub use message_parser::{content_blocks, parse_message};
pub use query::{Query, QueryConfig};
pub use transport::{SubprocessCliTransport, Transport};
