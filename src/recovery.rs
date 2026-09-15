// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    fs::OpenOptions,
    io,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::routing::LinkSpec;

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JournalData {
    #[serde(default)]
    direct_links: Vec<LinkSpec>,
}

pub struct RecoveryJournal {
    path: PathBuf,
}

impl RecoveryJournal {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load(&self) -> io::Result<Vec<LinkSpec>> {
        match std::fs::read_to_string(&self.path) {
            Ok(source) => toml::from_str::<JournalData>(&source)
                .map(|journal| journal.direct_links)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    pub fn replace(&self, links: &[LinkSpec]) -> io::Result<()> {
        if links.is_empty() {
            return match std::fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            };
        }

        let source = toml::to_string(&JournalData {
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
    use crate::routing::LinkEndpoint;

    static NEXT_PATH: AtomicUsize = AtomicUsize::new(0);

    fn journal() -> RecoveryJournal {
        RecoveryJournal::new(std::env::temp_dir().join(format!(
            "loudnessd-recovery-{}-{}.toml",
            std::process::id(),
            NEXT_PATH.fetch_add(1, Ordering::Relaxed)
        )))
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
        let journal = journal();
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
        assert!(journal().load().unwrap().is_empty());
    }
}
