// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    ffi::{CStr, CString},
    fmt,
    marker::PhantomData,
    ptr::NonNull,
    rc::Rc,
};

use pipewire::{loop_::Loop, properties::properties, sys};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilterState {
    Error,
    Unconnected,
    Connecting,
    Paused,
    Streaming,
    Unknown(i32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilterCreateError {
    NameContainsNul,
    CreationFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortDirection {
    Input,
    Output,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortDescriptor {
    pub direction: PortDirection,
    pub name: String,
    pub channel: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortCreateError {
    FilterNotUnconnected,
    CreationFailed,
}

impl fmt::Display for FilterCreateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NameContainsNul => formatter.write_str("filter name contains a NUL byte"),
            Self::CreationFailed => formatter.write_str("PipeWire failed to create the filter"),
        }
    }
}

impl std::error::Error for FilterCreateError {}

impl fmt::Display for PortCreateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FilterNotUnconnected => {
                formatter.write_str("ports can only be added before the filter is connected")
            }
            Self::CreationFailed => formatter.write_str("PipeWire failed to create the port"),
        }
    }
}

impl std::error::Error for PortCreateError {}

static FILTER_EVENTS: sys::pw_filter_events = sys::pw_filter_events {
    version: sys::PW_VERSION_FILTER_EVENTS,
    destroy: None,
    state_changed: None,
    io_changed: None,
    param_changed: None,
    add_buffer: None,
    remove_buffer: None,
    process: None,
    drained: None,
    command: None,
};

fn filter_name(name: &str) -> Result<CString, FilterCreateError> {
    CString::new(name).map_err(|_| FilterCreateError::NameContainsNul)
}

fn raw_direction(direction: PortDirection) -> pipewire::spa::sys::spa_direction {
    match direction {
        PortDirection::Input => pipewire::spa::sys::SPA_DIRECTION_INPUT,
        PortDirection::Output => pipewire::spa::sys::SPA_DIRECTION_OUTPUT,
    }
}

pub struct UnconnectedFilter {
    raw: NonNull<sys::pw_filter>,
    ports: Vec<OwnedPort>,
    _main_thread_only: PhantomData<Rc<()>>,
}

struct OwnedPort {
    _raw: NonNull<std::ffi::c_void>,
    descriptor: PortDescriptor,
}

#[repr(C)]
struct PortData {
    _reserved: u8,
}

impl UnconnectedFilter {
    pub fn new(loop_: &Loop, name: &str) -> Result<Self, FilterCreateError> {
        let name = filter_name(name)?;
        let properties = properties! {
            "media.type" => "Audio",
            "media.category" => "Filter",
            "media.role" => "DSP",
            "node.autoconnect" => "false",
            "object.linger" => "false",
        };

        // SAFETY: The loop and static event table outlive the returned filter.
        // pw_filter_new_simple takes ownership of properties. No callback data is
        // supplied, and the handle remains confined to the main-loop thread.
        let raw = unsafe {
            sys::pw_filter_new_simple(
                loop_.as_raw_ptr(),
                name.as_ptr(),
                properties.into_raw(),
                &FILTER_EVENTS,
                std::ptr::null_mut(),
            )
        };
        let raw = NonNull::new(raw).ok_or(FilterCreateError::CreationFailed)?;
        Ok(Self {
            raw,
            ports: Vec::new(),
            _main_thread_only: PhantomData,
        })
    }

    pub fn add_mono_port(
        &mut self,
        direction: PortDirection,
        name: impl Into<String>,
        channel: impl Into<String>,
    ) -> Result<&PortDescriptor, PortCreateError> {
        if self.state().0 != FilterState::Unconnected {
            return Err(PortCreateError::FilterNotUnconnected);
        }
        let descriptor = PortDescriptor {
            direction,
            name: name.into(),
            channel: channel.into(),
        };
        let properties = properties! {
            "format.dsp" => "32 bit float mono audio",
            "port.name" => descriptor.name.as_str(),
            "audio.channel" => descriptor.channel.as_str(),
        };
        let direction = raw_direction(direction);

        // SAFETY: raw is an unconnected filter owned by self. PipeWire takes
        // ownership of properties and keeps the returned port data alive until
        // the owning filter is destroyed.
        let raw = unsafe {
            sys::pw_filter_add_port(
                self.raw.as_ptr(),
                direction,
                sys::pw_filter_port_flags_PW_FILTER_PORT_FLAG_MAP_BUFFERS,
                std::mem::size_of::<PortData>(),
                properties.into_raw(),
                std::ptr::null_mut(),
                0,
            )
        };
        let raw = NonNull::new(raw).ok_or(PortCreateError::CreationFailed)?;
        self.ports.push(OwnedPort {
            _raw: raw,
            descriptor,
        });
        Ok(&self
            .ports
            .last()
            .expect("the new port was just stored")
            .descriptor)
    }

    pub fn ports(&self) -> impl ExactSizeIterator<Item = &PortDescriptor> {
        self.ports.iter().map(|port| &port.descriptor)
    }

    pub fn node_id(&self) -> Option<u32> {
        // SAFETY: raw is owned by self and stays valid until Drop.
        let node_id = unsafe { sys::pw_filter_get_node_id(self.raw.as_ptr()) };
        (node_id != sys::PW_ID_ANY).then_some(node_id)
    }

    pub fn state(&self) -> (FilterState, Option<String>) {
        let mut error = std::ptr::null();
        // SAFETY: raw is owned by self and stays valid until Drop.
        let state = unsafe { sys::pw_filter_get_state(self.raw.as_ptr(), &mut error) };
        let error = if error.is_null() {
            None
        } else {
            // PipeWire owns this error string for the lifetime of the state.
            unsafe { CStr::from_ptr(error) }
                .to_str()
                .ok()
                .map(str::to_owned)
        };
        (FilterState::from_raw(state), error)
    }
}

impl FilterState {
    fn from_raw(state: sys::pw_filter_state) -> Self {
        match state {
            sys::pw_filter_state_PW_FILTER_STATE_ERROR => Self::Error,
            sys::pw_filter_state_PW_FILTER_STATE_UNCONNECTED => Self::Unconnected,
            sys::pw_filter_state_PW_FILTER_STATE_CONNECTING => Self::Connecting,
            sys::pw_filter_state_PW_FILTER_STATE_PAUSED => Self::Paused,
            sys::pw_filter_state_PW_FILTER_STATE_STREAMING => Self::Streaming,
            state => Self::Unknown(state),
        }
    }
}

impl Drop for UnconnectedFilter {
    fn drop(&mut self) {
        // SAFETY: self uniquely owns raw, and no callback data can outlive it.
        unsafe { sys::pw_filter_destroy(self.raw.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_known_filter_state() {
        assert_eq!(
            FilterState::from_raw(sys::pw_filter_state_PW_FILTER_STATE_ERROR),
            FilterState::Error
        );
        assert_eq!(FilterState::from_raw(42), FilterState::Unknown(42));
    }

    #[test]
    fn rejects_invalid_names_before_touching_pipewire() {
        assert_eq!(
            filter_name("invalid\0name"),
            Err(FilterCreateError::NameContainsNul)
        );
    }

    #[test]
    fn maps_port_directions_to_the_pipewire_abi() {
        assert_eq!(
            raw_direction(PortDirection::Input),
            pipewire::spa::sys::SPA_DIRECTION_INPUT
        );
        assert_eq!(
            raw_direction(PortDirection::Output),
            pipewire::spa::sys::SPA_DIRECTION_OUTPUT
        );
    }

    #[test]
    #[ignore = "requires a live PipeWire user session"]
    fn live_unconnected_filter_owns_ports_without_registering_a_node() {
        let main_loop = pipewire::main_loop::MainLoopRc::new(None).unwrap();
        let mut filter = UnconnectedFilter::new(main_loop.loop_(), "loudnessd-test").unwrap();
        filter
            .add_mono_port(PortDirection::Input, "input_FL", "FL")
            .unwrap();
        filter
            .add_mono_port(PortDirection::Output, "output_FL", "FL")
            .unwrap();

        assert_eq!(filter.state().0, FilterState::Unconnected);
        assert_eq!(filter.ports().len(), 2);
        assert_eq!(filter.node_id(), None);
    }
}
