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

impl fmt::Display for FilterCreateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NameContainsNul => formatter.write_str("filter name contains a NUL byte"),
            Self::CreationFailed => formatter.write_str("PipeWire failed to create the filter"),
        }
    }
}

impl std::error::Error for FilterCreateError {}

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

pub struct UnconnectedFilter {
    raw: NonNull<sys::pw_filter>,
    _main_thread_only: PhantomData<Rc<()>>,
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
            _main_thread_only: PhantomData,
        })
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
}
