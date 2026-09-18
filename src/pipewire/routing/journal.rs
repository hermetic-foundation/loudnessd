// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    fs::OpenOptions,
    io,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::LinkSpec;

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JournalData {
    #[serde(default)]
    server_cookie: u32,
    #[serde(default)]
    direct_links: Vec<LinkSpec>,
}

pub struct RecoveryJournal {
    path: PathBuf,
    server_cookie: u32,
}

impl RecoveryJournal {
    pub fn new(path: impl Into<PathBuf>, server_cookie: u32) -> Self {
        Self {
            path: path.into(),
            server_cookie,
        }
    }

    pub fn load(&self) -> io::Result<Vec<LinkSpec>> {
        match std::fs::read_to_string(&self.path) {
            Ok(source) => {
                let journal = toml::from_str::<JournalData>(&source)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                if journal.server_cookie == self.server_cookie {
                    Ok(journal.direct_links)
                } else {
                    self.remove()?;
                    Ok(Vec::new())
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    pub fn replace(&self, links: &[LinkSpec]) -> io::Result<()> {
        if links.is_empty() {
            return self.remove();
        }

        let source = toml::to_string(&JournalData {
            server_cookie: self.server_cookie,
            direct_links: links.to_vec(),
        })
        .map_err(io::Error::other)?;
        let temporary = temporary_path(&self.path);
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(source.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(temporary, &self.path)
    }

    fn remove(&self) -> io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

pub fn path_for_socket(socket_path: &Path) -> PathBuf {
    socket_path.with_file_name("loudnessd-routes.toml")
}

fn temporary_path(path: &Path) -> PathBuf {
    path.with_extension(format!("tmp-{}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use std::{
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use super::*;
    use crate::pipewire::routing::LinkEndpoint;

    static NEXT_PATH: AtomicUsize = AtomicUsize::new(0);

    fn journal(server_cookie: u32) -> RecoveryJournal {
        RecoveryJournal::new(
            std::env::temp_dir().join(format!(
                "loudnessd-recovery-{}-{}.toml",
                std::process::id(),
                NEXT_PATH.fetch_add(1, Ordering::Relaxed)
            )),
            server_cookie,
        )
    }

    fn link() -> LinkSpec {
        LinkSpec {
            output: LinkEndpoint {
                node_id: 10,
                port_id: 11,
            },
            input: LinkEndpoint {
                node_id: 20,
                port_id: 21,
            },
        }
    }

    #[test]
    fn atomically_round_trips_private_route_state() {
        let journal = journal(42);
        journal.replace(&[link()]).unwrap();

        assert_eq!(journal.load().unwrap(), [link()]);
        assert_eq!(
            std::fs::metadata(&journal.path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        journal.replace(&[]).unwrap();
        assert!(!journal.path.exists());
    }

    #[test]
    fn missing_journal_is_an_empty_recovery_set() {
        assert!(journal(42).load().unwrap().is_empty());
    }

    #[test]
    fn discards_routes_from_a_different_pipewire_server() {
        let journal = journal(42);
        journal.replace(&[link()]).unwrap();
        let replacement_server = RecoveryJournal::new(&journal.path, 43);

        assert!(replacement_server.load().unwrap().is_empty());
        assert!(!journal.path.exists());
    }
}
