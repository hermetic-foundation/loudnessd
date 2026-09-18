// SPDX-License-Identifier: AGPL-3.0-or-later

#[test]
fn normalization_compatibility_paths_remain_available() {
    fn type_is_available<T>() {}

    type_is_available::<loudnessd::control::ControllerConfig>();
    type_is_available::<loudnessd::gain::GainStage>();
    type_is_available::<loudnessd::meter::LoudnessMeter>();
    type_is_available::<loudnessd::stream_control::StreamControl>();

    type_is_available::<loudnessd::normalization::control::ControllerConfig>();
    type_is_available::<loudnessd::normalization::dsp::gain::GainStage>();
    type_is_available::<loudnessd::normalization::dsp::meter::LoudnessMeter>();
    type_is_available::<loudnessd::normalization::stream::StreamControl>();
}

#[test]
fn pipewire_compatibility_paths_remain_available() {
    fn type_is_available<T>() {}

    type_is_available::<loudnessd::pipewire_backend::GraphState>();
    type_is_available::<loudnessd::pipewire_filter::FilterState>();
    type_is_available::<loudnessd::pipewire_links::OwnedLinks>();
    type_is_available::<loudnessd::pipewire_route_backend::PipewireRouteError>();
    type_is_available::<loudnessd::recovery::RecoveryJournal>();
    type_is_available::<loudnessd::route_transaction::TransitionOperation>();
    type_is_available::<loudnessd::routing::RouteHealth>();

    type_is_available::<loudnessd::pipewire::graph::GraphState>();
    type_is_available::<loudnessd::pipewire::filter::FilterState>();
    type_is_available::<loudnessd::pipewire::routing::links::OwnedLinks>();
    type_is_available::<loudnessd::pipewire::routing::backend::PipewireRouteError>();
    type_is_available::<loudnessd::pipewire::routing::journal::RecoveryJournal>();
    type_is_available::<loudnessd::pipewire::routing::transaction::TransitionOperation>();
    type_is_available::<loudnessd::pipewire::routing::RouteHealth>();
}

#[test]
fn daemon_config_compatibility_path_remains_available() {
    fn type_is_available<T>() {}

    type_is_available::<loudnessd::runtime_config::RuntimeConfig>();
    type_is_available::<loudnessd::daemon::runtime_config::RuntimeConfig>();
}
