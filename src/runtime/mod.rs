//! The live runtime: driving the `claude` CLI over the stream-json protocol.
//!
//! Ported from the Python `_internal/` runtime (`message_parser`, `transport`,
//! `query`) plus the public `query` / `client` entry points. Built
//! incrementally; this module currently exposes the message parser.

pub mod message_parser;
pub mod query;
pub mod transport;

pub use message_parser::parse_message;
pub use query::{Query, QueryConfig};
pub use transport::{SubprocessCliTransport, Transport};
