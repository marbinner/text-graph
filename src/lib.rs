#![forbid(unsafe_code)]

//! text-graph — a markdown vault as a typed graph.
//!
//! Pipeline: [`vault::scan`] walks and parses files (extraction, no global
//! state) → [`graph::build`] constructs the Contains tree from the directory
//! structure and calls [`resolve`] to turn wikilink strings into typed edges
//! → [`stats`] reports on the result.

pub mod graph;
pub mod layout;
pub mod resolve;
pub mod stats;
pub mod vault;
