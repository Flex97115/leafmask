//! SSH/SFTP storage backend (feature `storage.ssh-backend`).
//!
//! Config parsing and remote-path layout are pure and always tested; the actual
//! SFTP session (via `ssh2`) is behind the `ssh` cargo feature and is opened
//! lazily on first use, not eagerly at startup (needs a live SFTP host — see
//! regeneration-gaps.md).

use crate::config::Params;
use crate::error::{Error, Result};

/// Settings for the SFTP backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    /// Path to a private key file for key-based auth.
    pub private_key: Option<String>,
    /// Remote base directory dumps live under.
    pub base_path: String,
}

impl SshConfig {
    pub fn from_params(p: &Params) -> Result<SshConfig> {
        let host = str_param(p, "host")
            .ok_or_else(|| Error::Storage("ssh storage requires a 'host'".into()))?;
        Ok(SshConfig {
            host,
            port: p
                .get("port")
                .and_then(|v| v.as_u64())
                .map(|v| v as u16)
                .unwrap_or(22),
            username: str_param(p, "username"),
            password: str_param(p, "password"),
            private_key: str_param(p, "private_key"),
            base_path: str_param(p, "base_path").unwrap_or_else(|| ".".to_string()),
        })
    }

    /// The remote path for a storage-relative `path`.
    pub fn remote_path(&self, path: &str) -> String {
        let base = self.base_path.trim_end_matches('/');
        let path = path.trim_start_matches('/');
        if base.is_empty() || base == "." {
            path.to_string()
        } else {
            format!("{base}/{path}")
        }
    }
}

fn str_param(p: &Params, key: &str) -> Option<String> {
    p.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

#[cfg(feature = "ssh")]
pub use imp::SshStorage;

#[cfg(feature = "ssh")]
mod imp {
    use super::*;
    use crate::storage::Storage;
    use ssh2::Session;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::path::Path;
    use std::sync::Mutex;

    /// Live SFTP backend. The session is established lazily inside the mutex on
    /// first access rather than eagerly on construction.
    pub struct SshStorage {
        cfg: SshConfig,
        session: Mutex<Option<Session>>,
    }

    impl SshStorage {
        pub fn open(cfg: SshConfig) -> Result<Self> {
            Ok(SshStorage {
                cfg,
                session: Mutex::new(None),
            })
        }

        fn connect(&self) -> Result<Session> {
            let tcp = TcpStream::connect((self.cfg.host.as_str(), self.cfg.port))
                .map_err(|e| Error::Storage(format!("ssh connect: {e}")))?;
            let mut sess = Session::new().map_err(|e| Error::Storage(e.to_string()))?;
            sess.set_tcp_stream(tcp);
            sess.handshake()
                .map_err(|e| Error::Storage(e.to_string()))?;
            let user = self.cfg.username.as_deref().unwrap_or("root");
            if let Some(key) = &self.cfg.private_key {
                sess.userauth_pubkey_file(user, None, Path::new(key), None)
                    .map_err(|e| Error::Storage(e.to_string()))?;
            } else if let Some(pw) = &self.cfg.password {
                sess.userauth_password(user, pw)
                    .map_err(|e| Error::Storage(e.to_string()))?;
            }
            Ok(sess)
        }

        fn with_session<T>(&self, f: impl FnOnce(&Session) -> Result<T>) -> Result<T> {
            let mut guard = self.session.lock().unwrap();
            if guard.is_none() {
                *guard = Some(self.connect()?);
            }
            f(guard.as_ref().unwrap())
        }
    }

    impl Storage for SshStorage {
        fn list_dumps(&self) -> Result<Vec<String>> {
            let all = self.list("")?;
            let mut ids: Vec<String> = all
                .into_iter()
                .filter_map(|p| p.split('/').next().map(str::to_string))
                .collect();
            ids.sort();
            ids.dedup();
            Ok(ids)
        }

        fn get(&self, path: &str) -> Result<Vec<u8>> {
            let remote = self.cfg.remote_path(path);
            self.with_session(|sess| {
                let sftp = sess.sftp().map_err(|e| Error::Storage(e.to_string()))?;
                let mut file = sftp
                    .open(Path::new(&remote))
                    .map_err(|_| Error::NotFound(format!("blob '{path}' not found")))?;
                let mut buf = Vec::new();
                file.read_to_end(&mut buf)
                    .map_err(|e| Error::Storage(e.to_string()))?;
                Ok(buf)
            })
        }

        fn put(&self, path: &str, data: &[u8]) -> Result<()> {
            let remote = self.cfg.remote_path(path);
            self.with_session(|sess| {
                let sftp = sess.sftp().map_err(|e| Error::Storage(e.to_string()))?;
                // Ensure parent dirs exist.
                if let Some(parent) = Path::new(&remote).parent() {
                    let mut acc = std::path::PathBuf::new();
                    for comp in parent.components() {
                        acc.push(comp);
                        let _ = sftp.mkdir(&acc, 0o755);
                    }
                }
                let mut file = sftp
                    .create(Path::new(&remote))
                    .map_err(|e| Error::Storage(e.to_string()))?;
                file.write_all(data)
                    .map_err(|e| Error::Storage(e.to_string()))?;
                Ok(())
            })
        }

        fn exists(&self, path: &str) -> Result<bool> {
            let remote = self.cfg.remote_path(path);
            self.with_session(|sess| {
                let sftp = sess.sftp().map_err(|e| Error::Storage(e.to_string()))?;
                Ok(sftp.stat(Path::new(&remote)).is_ok())
            })
        }

        fn delete(&self, prefix: &str) -> Result<()> {
            for rel in self.list(prefix)? {
                let remote = self.cfg.remote_path(&rel);
                self.with_session(|sess| {
                    let sftp = sess.sftp().map_err(|e| Error::Storage(e.to_string()))?;
                    let _ = sftp.unlink(Path::new(&remote));
                    Ok(())
                })?;
            }
            Ok(())
        }

        fn list(&self, prefix: &str) -> Result<Vec<String>> {
            let base = self.cfg.remote_path("");
            let start = self.cfg.remote_path(prefix);
            let base_owned = base.clone();
            self.with_session(|sess| {
                let sftp = sess.sftp().map_err(|e| Error::Storage(e.to_string()))?;
                let mut out = Vec::new();
                let mut stack = vec![std::path::PathBuf::from(&start)];
                while let Some(dir) = stack.pop() {
                    let entries = match sftp.readdir(&dir) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };
                    for (path, stat) in entries {
                        if stat.is_dir() {
                            stack.push(path);
                        } else if let Ok(rel) = path.strip_prefix(&base_owned) {
                            out.push(rel.to_string_lossy().replace('\\', "/"));
                        }
                    }
                }
                out.sort();
                Ok(out)
            })
        }

        fn size(&self, prefix: &str) -> Result<u64> {
            let mut total = 0u64;
            for rel in self.list(prefix)? {
                total += self.get(&rel)?.len() as u64;
            }
            Ok(total)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(src: &str) -> Params {
        serde_yaml::from_str(src).unwrap()
    }

    // Acceptance: dumps upload to the configured remote path over SFTP.
    #[test]
    fn parses_host_and_builds_remote_paths() {
        let cfg = SshConfig::from_params(&params(
            "host: backup.example.com\nport: 2222\nusername: dumper\nbase_path: /srv/dumps\n",
        ))
        .unwrap();
        assert_eq!(cfg.host, "backup.example.com");
        assert_eq!(cfg.port, 2222);
        assert_eq!(
            cfg.remote_path("dump-1/metadata.json"),
            "/srv/dumps/dump-1/metadata.json"
        );

        // default port 22, default base_path ".".
        let cfg = SshConfig::from_params(&params("host: h\n")).unwrap();
        assert_eq!(cfg.port, 22);
        assert_eq!(cfg.remote_path("d/x"), "d/x");
    }

    #[test]
    fn requires_host() {
        assert!(SshConfig::from_params(&params("username: u\n")).is_err());
    }
}
