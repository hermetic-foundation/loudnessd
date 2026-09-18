// SPDX-License-Identifier: AGPL-3.0-or-later

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod daemon;
pub mod ipc;
pub mod monitor;
pub mod normalization;
pub mod pipewire;
mod process_metrics;
pub mod runtime_config;
pub mod status;

// Compatibility exports for the v0.1 public module paths.
pub use self::pipewire::filter as pipewire_filter;
pub use self::pipewire::graph as pipewire_backend;
pub use self::pipewire::routing;
pub use self::pipewire::routing::backend as pipewire_route_backend;
pub use self::pipewire::routing::journal as recovery;
pub use self::pipewire::routing::links as pipewire_links;
pub use self::pipewire::routing::transaction as route_transaction;
pub use normalization::control::{
    ApplicationPolicy, ApplicationPolicyOverride, Controller, ControllerBank, ControllerConfig,
    ControllerConfigOverride, Decision, Observation, SignalDomain, StreamState, UserConfig,
};
pub use normalization::{control, gain, meter, stream_control};
