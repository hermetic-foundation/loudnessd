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
