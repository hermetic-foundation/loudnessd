// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    time::{Duration, Instant},
};

use ::pipewire::loop_::Timeout;
use serde::{Deserialize, Serialize};

use super::{Daemon, ManagedStream};
use crate::{
    normalization::{SignalDomain, stream::StreamControl},
    pipewire::{
        filter::ConnectedFilter,
        routing::{RouteHealth, route_health},
        volume::NodeVolume,
    },
};

pub(super) const SUPPORTED: bool = true;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct UserVolume {
    pub serial: u64,
    pub level: f32,
    pub muted: bool,
    pub restore: NodeVolume,
    #[serde(skip)]
    pub published: bool,
}

#[derive(Serialize, Deserialize)]
struct JournalData {
    server_cookie: u32,
    streams: HashMap<u32, UserVolume>,
}

pub(super) struct VolumeJournal {
    path: PathBuf,
    server_cookie: u32,
}

impl VolumeJournal {
    pub fn new(path: PathBuf, server_cookie: u32) -> Self {
        Self {
            path,
            server_cookie,
        }
    }

    fn load(&self) -> Result<HashMap<u32, UserVolume>, String> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
            Err(error) => return Err(error.to_string()),
        };
        let data: JournalData =
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        if data.server_cookie == self.server_cookie {
            Ok(data.streams)
        } else {
            Ok(HashMap::new())
        }
    }

    pub fn save(&self, streams: &HashMap<u32, UserVolume>) -> Result<(), String> {
        let bytes = serde_json::to_vec(&JournalData {
            server_cookie: self.server_cookie,
            streams: streams.clone(),
        })
        .map_err(|error| error.to_string())?;
        let temporary = self
            .path
            .with_extension(format!("tmp-{}", std::process::id()));
        let write = || -> std::io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &self.path)?;
            if let Some(parent) = self.path.parent() {
                std::fs::File::open(parent)?.sync_all()?;
            }
            Ok(())
        };
        write().map_err(|error| format!("volume recovery journal: {error}"))
    }
}

fn wait_filter(
    main_loop: &::pipewire::main_loop::MainLoopRc,
    filter: &ConnectedFilter,
    mode: u32,
) -> Result<(), String> {
    filter.handoff(mode);
    let deadline = Instant::now() + Duration::from_millis(500);
    while filter.handoff_ack() != mode && Instant::now() < deadline {
        main_loop
            .loop_()
            .iterate(Timeout::Finite(Duration::from_millis(5)));
    }
    if filter.handoff_ack() == mode {
        Ok(())
    } else {
        Err("filter did not acknowledge safe handoff (stream may be paused)".to_owned())
    }
}

impl Daemon {
    fn check_volume_identity(&self, node_id: u32, serial: u64) -> Result<(), String> {
        if self.graph.borrow().node_serials.get(&node_id) == Some(&serial) {
            Ok(())
        } else {
            Err("stream disappeared or node identity changed".to_owned())
        }
    }

    fn read_node_volume(&mut self, node_id: u32, serial: u64) -> Result<NodeVolume, String> {
        self.check_volume_identity(node_id, serial)?;
        self.volume_sequence = self
            .volume_sequence
            .checked_add(1)
            .ok_or("Props sequence exhausted")?;
        let sequence = self
            .graph_tracker
            .read_volume(&self.core, node_id, self.volume_sequence)?;
        self.graph.borrow_mut().volume_replies.remove(&node_id);
        self.graph
            .borrow_mut()
            .volume_requests
            .insert(node_id, sequence);
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            self.check_volume_identity(node_id, serial)?;
            let reply = self.graph.borrow_mut().volume_replies.remove(&node_id);
            if let Some(volume) = reply {
                self.graph.borrow_mut().volume_requests.remove(&node_id);
                return Ok(volume);
            }
            if Instant::now() >= deadline {
                return Err("timed out reading fresh node Props".to_owned());
            }
            self.main_loop
                .loop_()
                .iterate(Timeout::Finite(Duration::from_millis(5)));
        }
    }

    fn write_node_volume(
        &mut self,
        node_id: u32,
        serial: u64,
        desired: &NodeVolume,
    ) -> Result<(), String> {
        self.check_volume_identity(node_id, serial)?;
        self.graph_tracker
            .write_volume(&self.core, node_id, desired)?;
        if self.read_node_volume(node_id, serial)? == *desired {
            Ok(())
        } else {
            Err("node did not acknowledge requested volume/mute Props".to_owned())
        }
    }

    pub(super) fn set_user_volume(
        &mut self,
        node_id: u32,
        level: Option<f32>,
        muted: Option<bool>,
    ) -> Result<String, String> {
        if !SUPPORTED {
            return Err("post-normalization volume is disabled pending lifecycle qualification; no upstream controls changed".to_owned());
        }
        let Some(ManagedStream::Active {
            stream,
            filter,
            route,
            ..
        }) = self.managed.get(&node_id)
        else {
            return Err("node is not an active managed playback stream".to_owned());
        };
        if stream.domain != SignalDomain::Playback
            || route_health(route.plan(), &self.graph.borrow()) != RouteHealth::Healthy
        {
            return Err("node is not a healthy managed playback stream".to_owned());
        }
        let serial = *self
            .graph
            .borrow()
            .node_serials
            .get(&node_id)
            .ok_or("node has no stable serial")?;
        if let Some(previous) = self.user_volumes.get(&node_id) {
            if !previous.published {
                return Err("previous handoff needs recovery before further commands".to_owned());
            }
            self.check_volume_identity(node_id, previous.serial)?;
            let mut next = previous.clone();
            next.level = level.unwrap_or(next.level);
            next.muted = muted.unwrap_or(next.muted);
            next.restore = next.restore.controls(next.level, next.muted);
            let mut states = self.user_volumes.clone();
            states.insert(node_id, next.clone());
            self.volume_journal.save(&states)?;
            filter.set_user_gain(if next.muted { 0.0 } else { next.level.powi(3) })?;
            self.user_volumes = states;
            return Ok("ok\n".to_owned());
        }

        let original = self.read_node_volume(node_id, serial)?;
        let (old_level, old_muted) = original.logical_controls()?;
        let level = level.unwrap_or(old_level);
        let muted = muted.unwrap_or(old_muted);
        let filter = match self.managed.get(&node_id).unwrap() {
            ManagedStream::Active { filter, .. } => filter,
            _ => unreachable!(),
        };
        if let Err(error) = wait_filter(&self.main_loop, filter, 1) {
            filter.handoff(0);
            return Err(error);
        }
        let mut next = UserVolume {
            serial,
            level,
            muted,
            restore: original.controls(old_level, old_muted),
            published: false,
        };
        self.user_volumes.insert(node_id, next.clone());
        if let Err(error) = self.volume_journal.save(&self.user_volumes) {
            self.user_volumes.remove(&node_id);
            self.volume_filter(node_id).unwrap().handoff(0);
            return Err(error);
        }
        // Journal the previous logical controls until neutralization is confirmed.
        let migration = self.write_node_volume(node_id, serial, &original.controls(1.0, false));
        if let Err(error) = migration {
            self.restore_user_volume(node_id)?;
            return Err(error);
        }
        let filter = self.volume_filter(node_id).unwrap();
        // Discard real graph audio after Props readback before resetting the
        // meter. This drains pre-neutralization buffers and ordinary SPA ramps;
        // wall-clock sleep alone would not drain a paused graph.
        let start = filter.held_audio_ns();
        let deadline = Instant::now() + Duration::from_millis(500);
        while filter.held_audio_ns().saturating_sub(start) < 100_000_000
            && Instant::now() < deadline
        {
            self.main_loop
                .loop_()
                .iterate(Timeout::Finite(Duration::from_millis(5)));
        }
        if filter.held_audio_ns().saturating_sub(start) < 100_000_000 {
            self.restore_user_volume(node_id)?;
            return Err("graph paused during unity handoff".to_owned());
        }
        if let Err(error) = wait_filter(&self.main_loop, filter, 2) {
            self.restore_user_volume(node_id)?;
            return Err(error);
        }
        let stream_id = node_id.to_string();
        self.controllers
            .retain_streams(|domain, id| domain != SignalDomain::Playback || id != stream_id);
        self.retained_dynamics.remove(&node_id);
        if let Some(ManagedStream::Active { control, .. }) = self.managed.get_mut(&node_id) {
            *control = StreamControl::new(control.domain(), control.application_id(), stream_id);
        }
        next.restore = original.controls(level, muted);
        self.user_volumes.insert(node_id, next.clone());
        if let Err(error) = self.volume_journal.save(&self.user_volumes) {
            self.restore_user_volume(node_id)?;
            return Err(error);
        }
        let filter = self.volume_filter(node_id).unwrap();
        filter.set_user_gain(if muted { 0.0 } else { level.powi(3) })?;
        filter.handoff(0);
        next.published = true;
        self.user_volumes.insert(node_id, next);
        Ok("ok\n".to_owned())
    }

    fn volume_filter(&self, node_id: u32) -> Option<&ConnectedFilter> {
        self.managed.get(&node_id).map(|managed| match managed {
            ManagedStream::Active { filter, .. } | ManagedStream::Connecting { filter, .. } => {
                filter
            }
        })
    }

    pub(super) fn restore_user_volume(&mut self, node_id: u32) -> Result<(), String> {
        let Some(volume) = self.user_volumes.get(&node_id).cloned() else {
            return Ok(());
        };
        if self.graph.borrow().node_serials.get(&node_id) == Some(&volume.serial) {
            if let Some(filter) = self.volume_filter(node_id) {
                filter.handoff(1);
                // A paused filter cannot acknowledge a process callback. Its
                // first resumed callback will honor the hold before any gain.
                if filter.state().0 != crate::pipewire::filter::FilterState::Paused {
                    wait_filter(&self.main_loop, filter, 1)?;
                }
            }
            self.write_node_volume(node_id, volume.serial, &volume.restore)?;
            if let Some(filter) = self.volume_filter(node_id) {
                filter.set_user_gain(1.0)?;
                filter.handoff(0);
            }
        }
        let mut states = self.user_volumes.clone();
        states.remove(&node_id);
        self.volume_journal.save(&states)?;
        self.user_volumes = states;
        Ok(())
    }

    pub(super) fn recover_user_volumes(&mut self) -> Result<(), String> {
        self.user_volumes = self.volume_journal.load()?;
        if self.user_volumes.is_empty() {
            return Ok(());
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while self
            .user_volumes
            .keys()
            .any(|node| !self.graph.borrow().node_serials.contains_key(node))
            && Instant::now() < deadline
        {
            self.main_loop
                .loop_()
                .iterate(Timeout::Finite(Duration::from_millis(20)));
        }
        let nodes: Vec<_> = self.user_volumes.keys().copied().collect();
        for node in nodes {
            self.restore_user_volume(node)?;
        }
        Ok(())
    }
}
