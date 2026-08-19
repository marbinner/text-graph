#![forbid(unsafe_code)]

//! text-graph — a markdown vault as a typed graph.
//!
//! Pipeline: [`vault::scan`] walks and parses files (extraction, no global
//! state) → [`graph::build`] constructs the Contains tree from the directory
//! structure and calls [`resolve`] to turn wikilink strings into typed edges
//! → [`stats`] reports on the result. For the viewer, [`layout`] computes the
//! deterministic radial seed and [`sim`] relaxes it force-directed; the egui
//! shell lives in the binary (`src/app/`), keeping this library headless.

pub mod agents;
pub mod comm;
pub mod config;
pub mod create;
pub mod filetype;
pub mod graph;
#[cfg(feature = "gui")]
pub mod highlight;
pub mod keys;
pub mod layout;
pub mod mathtext;
pub mod mdview;
pub mod mirror;
pub mod resolve;
pub mod search;
pub mod sim;
pub mod state;
pub mod stats;
#[cfg(feature = "gui")]
pub mod thumb;
pub mod tmux;
pub mod vault;
pub mod weburl;
