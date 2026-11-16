// SPDX-License-Identifier: AGPL-3.0-or-later

use std::io::Cursor;

use ::pipewire::spa::{
    param::ParamType,
    pod::{
        Object, Pod, Property, Value, ValueArray, deserialize::PodDeserializer,
        serialize::PodSerializer,
    },
    sys,
};

/// Logical UI controls, not an estimate of the gain currently applied to samples.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NodeVolume {
    pub volume: Option<f32>,
    pub muted: Option<bool>,
    pub channels: Option<Vec<f32>>,
    pub soft_muted: Option<bool>,
    pub soft: Option<Vec<f32>>,
}

impl NodeVolume {
    pub fn from_pod(pod: &Pod) -> Option<Self> {
        let (_, Value::Object(object)) =
            PodDeserializer::deserialize_any_from(pod.as_bytes()).ok()?
        else {
            return None;
        };
        if object.type_ != sys::SPA_TYPE_OBJECT_Props || object.id != ParamType::Props.as_raw() {
            return None;
        }
        let mut result = Self {
            volume: None,
            muted: None,
            channels: None,
            soft_muted: None,
            soft: None,
        };
        for prop in object.properties {
            match (prop.key, prop.value) {
                (sys::SPA_PROP_volume, Value::Float(value)) => result.volume = Some(value),
                (sys::SPA_PROP_mute, Value::Bool(value)) => result.muted = Some(value),
                (sys::SPA_PROP_channelVolumes, Value::ValueArray(ValueArray::Float(values))) => {
                    result.channels = Some(values)
                }
                (sys::SPA_PROP_softMute, Value::Bool(value)) => result.soft_muted = Some(value),
                (sys::SPA_PROP_softVolumes, Value::ValueArray(ValueArray::Float(values))) => {
                    result.soft = Some(values)
                }
                _ => {}
            }
        }
        Some(result)
    }

    pub fn logical_controls(&self) -> Result<(f32, bool), String> {
        // channelVolumes is the logical mixer control. softVolumes may override
        // it in audioconvert; neither is used to undo gain in the source meter.
        let gain = if let Some(channels) = &self.channels {
            let first = *channels.first().ok_or("empty channelVolumes")?;
            if channels
                .iter()
                .any(|gain| !gain.is_finite() || (*gain - first).abs() > 0.000001)
            {
                return Err("asymmetric or invalid channelVolumes are not supported".to_owned());
            }
            first
        } else {
            self.volume.ok_or("node has no readable volume control")?
        };
        if !gain.is_finite() || !(0.0..=8.0).contains(&gain) {
            return Err("original volume is outside supported 0..2 UI range".to_owned());
        }
        Ok((
            gain.cbrt(),
            self.muted.ok_or("node has no readable mute control")?,
        ))
    }

    pub fn controls(&self, level: f32, muted: bool) -> Self {
        let gain = level.powi(3);
        Self {
            volume: self
                .volume
                .map(|_| if self.channels.is_some() { 1.0 } else { gain }),
            muted: self.muted.map(|_| muted),
            channels: self
                .channels
                .as_ref()
                .map(|values| vec![gain; values.len()]),
            soft_muted: self.soft_muted.map(|_| muted),
            soft: self.soft.as_ref().map(|values| {
                vec![
                    if self.channels.is_some() { gain } else { 1.0 };
                    self.channels
                        .as_ref()
                        .map_or(values.len(), Vec::len)
                        .max(values.len())
                ]
            }),
        }
    }

    pub fn serialize(&self) -> Result<Vec<u8>, String> {
        let mut properties = Vec::new();
        let mut push = |key, value| {
            properties.push(Property {
                key,
                flags: ::pipewire::spa::pod::PropertyFlags::empty(),
                value,
            })
        };
        if let Some(value) = self.volume {
            push(sys::SPA_PROP_volume, Value::Float(value));
        }
        if let Some(value) = self.muted {
            push(sys::SPA_PROP_mute, Value::Bool(value));
        }
        if let Some(values) = &self.channels {
            push(
                sys::SPA_PROP_channelVolumes,
                Value::ValueArray(ValueArray::Float(values.clone())),
            );
        }
        if let Some(value) = self.soft_muted {
            push(sys::SPA_PROP_softMute, Value::Bool(value));
        }
        if let Some(values) = &self.soft {
            push(
                sys::SPA_PROP_softVolumes,
                Value::ValueArray(ValueArray::Float(values.clone())),
            );
        }
        let object = Value::Object(Object {
            type_: sys::SPA_TYPE_OBJECT_Props,
            id: ParamType::Props.as_raw(),
            properties,
        });
        PodSerializer::serialize(Cursor::new(Vec::new()), &object)
            .map(|(cursor, _)| cursor.into_inner())
            .map_err(|error| format!("serialize Props: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn original() -> NodeVolume {
        NodeVolume {
            volume: Some(1.0),
            muted: Some(true),
            channels: Some(vec![0.125; 2]),
            soft_muted: Some(false),
            soft: Some(vec![1.0; 2]),
        }
    }

    #[test]
    fn props_roundtrip_and_soft_override_are_not_multiplied_into_ui_control() {
        let state = original();
        let bytes = state.serialize().unwrap();
        assert_eq!(
            NodeVolume::from_pod(Pod::from_bytes(&bytes).unwrap()),
            Some(state.clone())
        );
        assert_eq!(state.logical_controls().unwrap(), (0.5, true));
        let unity = state.controls(1.0, false);
        assert_eq!(unity.channels, Some(vec![1.0; 2]));
        assert_eq!(unity.soft, Some(vec![1.0; 2]));
        assert_eq!(unity.soft_muted, Some(false));
        let restored = state.controls(2.0, true);
        assert_eq!(restored.channels, Some(vec![8.0; 2]));
        assert_eq!(restored.soft, Some(vec![8.0; 2]));
    }

    #[test]
    fn rejects_missing_invalid_and_asymmetric_logical_controls() {
        let mut state = original();
        state.channels = Some(vec![0.1, 0.2]);
        assert!(state.logical_controls().is_err());
        state.channels = Some(vec![f32::NAN]);
        assert!(state.logical_controls().is_err());
        state.channels = None;
        state.volume = None;
        assert!(state.logical_controls().is_err());
    }
}
