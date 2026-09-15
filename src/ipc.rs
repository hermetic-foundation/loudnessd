// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    fs::Permissions,
    io::{self, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    time::Duration,
};

pub const SOCKET_NAME: &str = "loudnessd.sock";
const REQUEST_TIMEOUT: Duration = Duration::from_millis(250);

pub fn default_socket_path() -> Result<PathBuf, &'static str> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(|path| path.join(SOCKET_NAME))
        .ok_or("XDG_RUNTIME_DIR is unset or not absolute")
}

pub struct ControlServer {
    listener: UnixListener,
    path: PathBuf,
}

impl ControlServer {
    pub fn bind(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        match UnixStream::connect(&path) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "another loudnessd daemon is already listening",
                ));
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                ) =>
            {
                if error.kind() == io::ErrorKind::ConnectionRefused {
                    std::fs::remove_file(&path)?;
                }
            }
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok(Self { listener, path })
    }

    pub fn accept_request(&self) -> io::Result<Option<(UnixStream, String)>> {
        let (mut stream, _) = match self.listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(error) => return Err(error),
        };
        stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
        let mut request = String::new();
        stream.read_to_string(&mut request)?;
        Ok(Some((stream, request)))
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn send(path: &Path, arguments: &[String]) -> io::Result<String> {
    let mut stream = UnixStream::connect(path)?;
    stream.write_all(arguments.join("\t").as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

pub fn write_response(mut stream: UnixStream, response: &str) -> io::Result<()> {
    stream.write_all(response.as_bytes())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static NEXT_SOCKET: AtomicUsize = AtomicUsize::new(0);

    fn socket_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "loudnessd-ipc-test-{}-{}.sock",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn socket_is_private_and_removed_with_the_server() {
        let path = socket_path();
        let server = ControlServer::bind(&path).unwrap();

        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(server);
        assert!(!path.exists());
    }

    #[test]
    fn refuses_to_replace_a_live_daemon_socket() {
        let path = socket_path();
        let _server = ControlServer::bind(&path).unwrap();

        assert_eq!(
            ControlServer::bind(&path).err().unwrap().kind(),
            io::ErrorKind::AddrInUse
        );
    }

    #[test]
    fn incomplete_client_cannot_block_indefinitely() {
        let path = socket_path();
        let server = ControlServer::bind(&path).unwrap();
        let _client = UnixStream::connect(&path).unwrap();
        let started = std::time::Instant::now();

        let error = server.accept_request().unwrap_err();

        assert!(matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
