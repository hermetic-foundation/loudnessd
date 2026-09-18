// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod control;
pub mod dsp;
pub mod stream;

// Preserve the pre-0.1.1 module name while the canonical path becomes
// `normalization::stream`.
pub use stream as stream_control;

pub use control::{
    ApplicationPolicy, ApplicationPolicyOverride, Controller, ControllerBank, ControllerConfig,
    ControllerConfigOverride, Decision, Observation, SignalDomain, StreamState, UserConfig,
};
pub use dsp::{gain, meter};
