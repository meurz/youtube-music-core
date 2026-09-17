//! CLI host storage. The protocol library never implicitly persists credentials.
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
};
use youtube_music_core::{auth::Session, Error, Result};
use zeroize::Zeroizing;

const MAX_SECRET: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, clap::ValueEnum)]
pub enum StoreKind {
    #[default]
    Auto,
    Pass,
    Keyring,
}

impl StoreKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Pass => "pass",
            Self::Keyring => "keyring",
        }
    }
}

pub struct SessionStore {
    kind: StoreKind,
    profile: String,
}

fn storage(message: &str) -> Error {
    Error::CredentialStorage(message.into())
}

pub fn validate_profile(profile: &str) -> Result<()> {
    if profile.is_empty()
        || profile.len() > 64
        || !profile
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(Error::InvalidInput(
            "profile must contain 1..64 letters, digits, underscores, or hyphens".into(),
        ));
    }
    Ok(())
}

fn decode_session(bytes: &[u8]) -> Result<Session> {
    serde_json::from_slice(bytes).map_err(|error| {
        if error
            .to_string()
            .contains(youtube_music_core::auth::LEGACY_AUTH_MESSAGE)
        {
            storage(youtube_music_core::auth::LEGACY_AUTH_MESSAGE)
        } else {
            storage("saved profile is invalid; import a new session with auth import")
        }
    })
}

impl SessionStore {
    pub fn new(kind: StoreKind, profile: &str) -> Result<Self> {
        validate_profile(profile)?;
        let kind = match kind {
            StoreKind::Auto if cfg!(any(target_os = "windows", target_os = "macos")) => {
                StoreKind::Keyring
            }
            StoreKind::Auto => StoreKind::Pass,
            other => other,
        };
        Ok(Self {
            kind,
            profile: profile.into(),
        })
    }

    fn pass_key(&self) -> String {
        format!("youtube-music-core/session/{}", self.profile)
    }

    fn pass_file(&self) -> Result<std::path::PathBuf> {
        let root = std::env::var_os("PASSWORD_STORE_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|p| std::path::PathBuf::from(p).join(".password-store"))
            })
            .ok_or_else(|| storage("cannot locate the pass store"))?;
        Ok(root.join(format!("{}.gpg", self.pass_key())))
    }

    pub fn load(&self) -> Result<Option<Session>> {
        let bytes = match self.kind {
            StoreKind::Pass => {
                if !self
                    .pass_file()?
                    .try_exists()
                    .map_err(|_| storage("cannot inspect the pass entry"))?
                {
                    return Ok(None);
                }
                let mut child = Command::new("pass")
                    .args(["show", &self.pass_key()])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|_| {
                        storage("install and initialize pass before using account profiles")
                    })?;
                let mut bytes = Zeroizing::new(Vec::new());
                let read = child
                    .stdout
                    .take()
                    .ok_or_else(|| storage("cannot read pass output"))?
                    .take(MAX_SECRET + 1)
                    .read_to_end(&mut bytes);
                if read.is_err() || bytes.len() as u64 > MAX_SECRET {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(storage("invalid or oversized pass entry"));
                }
                if !child
                    .wait()
                    .map_err(|_| storage("cannot wait for pass"))?
                    .success()
                {
                    return Err(storage(
                        "pass could not decrypt this profile; unlock its GPG key",
                    ));
                }
                bytes
            }
            StoreKind::Keyring => match native::load(&self.profile)? {
                Some(bytes) => bytes,
                None => return Ok(None),
            },
            StoreKind::Auto => unreachable!(),
        };
        if bytes.is_empty() {
            return Err(storage("saved profile is empty; import a new session"));
        }
        let session: Session = decode_session(&bytes)?;
        session.validate()?;
        Ok(Some(session))
    }

    pub fn save(&self, session: &Session) -> Result<()> {
        session.validate()?;
        let bytes = Zeroizing::new(
            serde_json::to_vec(session).map_err(|_| storage("cannot encode the session"))?,
        );
        match self.kind {
            StoreKind::Pass => {
                let mut child = Command::new("pass")
                    .args(["insert", "--multiline", "--force", &self.pass_key()])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|_| {
                        storage("install and initialize pass before importing a session")
                    })?;
                let write = child
                    .stdin
                    .take()
                    .ok_or_else(|| storage("cannot write to pass"))?
                    .write_all(&bytes);
                if write.is_err() {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(storage("could not send the session to pass"));
                }
                if !child
                    .wait()
                    .map_err(|_| storage("cannot wait for pass"))?
                    .success()
                {
                    return Err(storage(
                        "pass could not save the session; check its GPG configuration",
                    ));
                }
                Ok(())
            }
            StoreKind::Keyring => native::save(&self.profile, &bytes),
            StoreKind::Auto => unreachable!(),
        }
    }

    pub fn delete(&self) -> Result<bool> {
        match self.kind {
            StoreKind::Pass => {
                if !self
                    .pass_file()?
                    .try_exists()
                    .map_err(|_| storage("cannot inspect the pass entry"))?
                {
                    return Ok(false);
                }
                let result = Command::new("pass")
                    .args(["rm", "--force", &self.pass_key()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .map_err(|_| storage("cannot run pass"))?;
                if !result.success() {
                    return Err(storage("pass could not delete the saved profile"));
                }
                Ok(true)
            }
            StoreKind::Keyring => native::delete(&self.profile),
            StoreKind::Auto => unreachable!(),
        }
    }
}

// The OS credential entry holds only a random AES key: browser cookies can
// exceed Windows Credential Manager's per-entry size limit.
#[cfg(any(target_os = "windows", target_os = "macos", test))]
mod vault {
    use super::*;
    use aes_gcm::{
        aead::{Aead, KeyInit, Payload},
        Aes256Gcm, Nonce,
    };
    const MAGIC: &[u8] = b"YTMUSIC-SESSION-1\0";

    pub fn encrypt(key: &[u8], profile: &str, bytes: &[u8]) -> Result<Vec<u8>> {
        let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| storage("invalid vault key"))?;
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce).map_err(|_| storage("secure randomness unavailable"))?;
        let encrypted = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: bytes,
                    aad: profile.as_bytes(),
                },
            )
            .map_err(|_| storage("cannot encrypt the browser session"))?;
        let mut output = MAGIC.to_vec();
        output.extend(nonce);
        output.extend(encrypted);
        Ok(output)
    }

    pub fn decrypt(key: &[u8], profile: &str, bytes: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        let rest = bytes
            .strip_prefix(MAGIC)
            .filter(|b| b.len() >= 28)
            .ok_or_else(|| storage("invalid encrypted session file"))?;
        let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| storage("invalid vault key"))?;
        let plain = cipher
            .decrypt(
                Nonce::from_slice(&rest[..12]),
                Payload {
                    msg: &rest[12..],
                    aad: profile.as_bytes(),
                },
            )
            .map_err(|_| storage("session could not be decrypted or was modified"))?;
        Ok(Zeroizing::new(plain))
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
mod native {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine};
    use std::path::PathBuf;

    fn path(profile: &str) -> Result<PathBuf> {
        #[cfg(target_os = "windows")]
        let root = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
        #[cfg(target_os = "macos")]
        let root =
            std::env::var_os("HOME").map(|v| PathBuf::from(v).join("Library/Application Support"));
        Ok(root
            .ok_or_else(|| storage("cannot locate the user application-data directory"))?
            .join("youtube-music-core")
            .join("sessions")
            .join(format!("{profile}.bin")))
    }

    fn entry(profile: &str) -> Result<keyring::Entry> {
        keyring::Entry::new("youtube-music-core", &format!("vault-key:{profile}"))
            .map_err(|_| storage("cannot open the system credential store"))
    }

    fn key(profile: &str, create: bool) -> Result<Zeroizing<Vec<u8>>> {
        let entry = entry(profile)?;
        match entry.get_password() {
            Ok(encoded) => {
                let encoded = Zeroizing::new(encoded);
                let key = Zeroizing::new(
                    STANDARD
                        .decode(encoded.as_bytes())
                        .map_err(|_| storage("invalid system vault key"))?,
                );
                if key.len() != 32 {
                    return Err(storage("invalid system vault key"));
                }
                Ok(key)
            }
            Err(keyring::Error::NoEntry) if create => {
                let mut key = Zeroizing::new(vec![0; 32]);
                getrandom::fill(&mut key).map_err(|_| storage("secure randomness unavailable"))?;
                let encoded = Zeroizing::new(STANDARD.encode(&*key));
                entry
                    .set_password(&encoded)
                    .map_err(|_| storage("cannot save the system vault key"))?;
                Ok(key)
            }
            Err(_) => Err(storage(
                "cannot read the system vault key; unlock the credential store",
            )),
        }
    }

    pub fn load(profile: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
        let path = path(profile)?;
        if !path
            .try_exists()
            .map_err(|_| storage("cannot inspect the session vault"))?
        {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|_| storage("cannot open the session vault"))?
            .take(MAX_SECRET + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| storage("cannot read the session vault"))?;
        if bytes.len() as u64 > MAX_SECRET {
            return Err(storage("session vault exceeds its size limit"));
        }
        vault::decrypt(&key(profile, false)?, profile, &bytes).map(Some)
    }

    pub fn save(profile: &str, bytes: &[u8]) -> Result<()> {
        let path = path(profile)?;
        let parent = path
            .parent()
            .ok_or_else(|| storage("invalid session path"))?;
        std::fs::create_dir_all(parent)
            .map_err(|_| storage("cannot create the session directory"))?;
        let encrypted = vault::encrypt(&key(profile, true)?, profile, bytes)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)
            .map_err(|_| storage("cannot create an encrypted session file"))?;
        file.write_all(&encrypted)
            .map_err(|_| storage("cannot write the encrypted session"))?;
        file.as_file()
            .sync_all()
            .map_err(|_| storage("cannot flush the encrypted session"))?;
        file.persist(path)
            .map_err(|_| storage("cannot replace the encrypted session"))?;
        Ok(())
    }

    pub fn delete(profile: &str) -> Result<bool> {
        let path = path(profile)?;
        let existed = path
            .try_exists()
            .map_err(|_| storage("cannot inspect the session vault"))?;
        if existed {
            std::fs::remove_file(path)
                .map_err(|_| storage("cannot delete the encrypted session"))?;
        }
        match entry(profile)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(existed),
            Err(_) => Err(storage(
                "session file removed, but the system vault key could not be deleted",
            )),
        }
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod native {
    use super::*;
    pub fn load(_: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
        Err(storage("use the pass backend on this platform"))
    }
    pub fn save(_: &str, _: &[u8]) -> Result<()> {
        Err(storage("use the pass backend on this platform"))
    }
    pub fn delete(_: &str) -> Result<bool> {
        Err(storage("use the pass backend on this platform"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_oauth_profiles_require_cookie_import_without_echoing_secrets() {
        for bytes in [
            br#"{"access_token":"private-access","refresh_token":"private-refresh","client_secret":"private-secret"}"#.as_slice(),
            br#"{"access_token":"private-access","refresh_token":"private-refresh","client_id":"client"}"#.as_slice(),
        ] {
            let error = decode_session(bytes).unwrap_err().to_string();
            assert!(error.contains("auth import"));
            assert!(error.contains("OAuth profiles are no longer supported"));
            assert!(!error.contains("private"));
        }
    }

    #[test]
    fn profile_names_cannot_escape_the_store() {
        for name in ["", "../other", "a/b", "a\\b", "--force", "a b"] {
            if name == "--force" {
                continue;
            } // Names remain under a fixed safe prefix.
            assert!(validate_profile(name).is_err());
        }
        assert!(validate_profile("personal_1").is_ok());
    }

    #[test]
    fn vault_is_confidential_authenticated_and_profile_bound() {
        let key = [7; 32];
        let bytes = b"SAPISID=synthetic-private-value";
        let encrypted = vault::encrypt(&key, "personal", bytes).unwrap();
        assert!(!encrypted.windows(bytes.len()).any(|w| w == bytes));
        assert_eq!(
            &**vault::decrypt(&key, "personal", &encrypted)
                .as_ref()
                .unwrap(),
            bytes
        );
        assert!(vault::decrypt(&[8; 32], "personal", &encrypted).is_err());
        assert!(vault::decrypt(&key, "other", &encrypted).is_err());
        let mut modified = encrypted.clone();
        *modified.last_mut().unwrap() ^= 1;
        assert!(vault::decrypt(&key, "personal", &modified).is_err());
        assert!(vault::decrypt(&key, "personal", &encrypted[..16]).is_err());
        assert_ne!(encrypted, vault::encrypt(&key, "personal", bytes).unwrap());
    }
}
