//! The Xenith compiler as an MCP server.
//!
//! This is the compiler–model protocol made literal: the same questions the
//! CLI answers — goals, check, type-at, producers, fmt, explain — exposed as
//! tools an agent calls directly, over MCP's stdio transport.
//!
//! The protocol layer is written by hand rather than taken from an SDK. The
//! surface we need is three request kinds over newline-delimited JSON-RPC;
//! a synchronous compiler gains nothing from an async runtime, and this
//! project's dependency list is short on purpose. If the server ever needs
//! subscriptions or sampling, that is the day to take the SDK.

pub mod server;
