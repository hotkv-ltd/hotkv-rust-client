// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Typed command wrappers, grouped by command family. Every submodule
//! adds methods straight onto [`crate::Client`] through its own `impl
//! Client` block; the families with several related subcommands (`TAG.*`,
//! `GCRA.*`, `VSIM.*`, `RATELIMIT.*`, `TS.*`, `FEATURE.*`,
//! `OBSERVE`/`ALERTS`) are reached through a small accessor type, e.g.
//! `client.tag()`, re-exported here.

pub mod bitmap;
pub mod enterprise;
pub mod geo;
pub mod hashes;
pub mod hotkv_core;
pub mod keys;
pub mod lists;
pub mod scripting;
pub mod server;
pub mod sets;
pub mod streams;
pub mod strings;
pub mod zsets;

pub use enterprise::{AlertsCommands, FeatureCommands, ObserveCommands, RateLimitCommands, TsCommands};
pub use hotkv_core::{GcraCommands, TagCommands, VsimCommands};
