// SPDX-License-Identifier: AGPL-3.0-or-later

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod daemon;
pub mod ipc;
pub mod monitor;
pub mod normalization;
pub mod pipewire_backend;
pub mod pipewire_filter;
pub mod pipewire_links;
pub mod pipewire_route_backend;
mod process_metrics;
pub mod recovery;
pub mod route_transaction;
pub mod routing;
pub mod runtime_config;
pub mod status;

// Compatibility exports for the v0.1 public module paths.
pub use normalization::control::{
    ApplicationPolicy, ApplicationPolicyOverride, Controller, ControllerBank, ControllerConfig,
    ControllerConfigOverride, Decision, Observation, SignalDomain, StreamState, UserConfig,
};
pub use normalization::{control, gain, meter, stream_control};
