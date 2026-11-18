//! Where the bot keeps its login between runs, and how it is written so a crash cannot lose it.
//!
//! Twitch replaces the refresh token every time it is used, so the new one must reach the disk before anything
//! else happens: the file is written to a temporary name, flushed to the disk, then renamed over the old one.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use pyxeetb_twitch::oauth::Tokens;

use crate::error::{Context, Result};

const FILE_NAME: &str = "tokens.json";

/// The current time as Unix seconds. A clock set before 1970 counts as 0, which only makes a token look expired.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The state directory: the explicit choice, else `$XDG_CONFIG_HOME/pyxeetb`, else `$HOME/.config/pyxeetb`,
/// else `./pyxeetb-state`. `env` looks up an environment variable, so tests need not touch the real one.
pub fn state_dir(explicit: Option<&str>, env: impl Fn(&str) -> Option<String>) -> PathBuf {
    if let Some(dir) = explicit
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    let non_empty = |name: &str| env(name).filter(|v| !v.is_empty());
    if let Some(dir) = non_empty("PYXEETB_STATE_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(dir) = non_empty("XDG_CONFIG_HOME") {
        return Path::new(&dir).join("pyxeetb");
    }
    non_empty("HOME").map_or_else(
        || PathBuf::from("pyxeetb-state"),
        |home| Path::new(&home).join(".config").join("pyxeetb"),
    )
}

#[derive(Debug, Clone)]
pub struct TokenStore {
    dir: PathBuf,
}

impl TokenStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(FILE_NAME)
    }

    /// # Errors
    /// When the file exists but cannot be read or is not a token file. A missing file is `Ok(None)`.
    pub fn load(&self) -> Result<Option<Tokens>> {
        match fs::read_to_string(self.path()) {
            Ok(text) => Ok(Some(
                serde_json::from_str(&text).context("the token file is not valid")?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).context("reading the token file"),
        }
    }

    /// Write the tokens so that either the old file or the new one survives a crash, readable by this user only.
    ///
    /// # Errors
    /// When the directory or file cannot be written.
    pub fn save(&self, tokens: &Tokens) -> Result<()> {
        fs::create_dir_all(&self.dir).context("creating the state directory")?;
        let temp = self.dir.join(format!("{FILE_NAME}.tmp"));
        let json = serde_json::to_string_pretty(tokens).context("writing the token file")?;
        let mut file = create_private(&temp).context("creating the token file")?;
        file.write_all(json.as_bytes())
            .context("writing the token file")?;
        file.write_all(b"\n").context("writing the token file")?;
        file.sync_all().context("flushing the token file to disk")?;
        drop(file);
        fs::rename(&temp, self.path()).context("replacing the token file")?;
        // Make the rename itself durable. Not every platform can open a directory, and that is fine.
        if let Ok(dir) = File::open(&self.dir) {
            let _ = dir.sync_all();
        }
        Ok(())
    }
}

#[cfg(unix)]
fn create_private(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

#[cfg(test)]
mod tests {
    use pyxeetb_twitch::Secret;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pyxeetb-test-{name}-{}-{}",
            std::process::id(),
            unix_now()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn tokens() -> Tokens {
        Tokens {
            client_id: "cid".into(),
            user_id: "1".into(),
            login: "pylxyr".into(),
            access_token: Secret::new("a"),
            refresh_token: Some(Secret::new("r")),
            scopes: vec!["chat:read".into()],
            expires_at: 99,
        }
    }

    #[test]
    fn a_missing_file_is_none_and_a_saved_one_comes_back() {
        let store = TokenStore::new(temp_dir("roundtrip"));
        assert_eq!(store.load().unwrap(), None);
        store.save(&tokens()).unwrap();
        assert_eq!(store.load().unwrap(), Some(tokens()));
        fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
    }

    #[test]
    fn saving_again_replaces_the_file_and_leaves_no_temporary() {
        let dir = temp_dir("replace");
        let store = TokenStore::new(&dir);
        store.save(&tokens()).unwrap();
        let mut newer = tokens();
        newer.expires_at = 5_000;
        store.save(&newer).unwrap();
        assert_eq!(store.load().unwrap().unwrap().expires_at, 5_000);
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [FILE_NAME]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let store = TokenStore::new(temp_dir("mode"));
        store.save(&tokens()).unwrap();
        let mode = fs::metadata(store.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
    }

    #[test]
    fn a_damaged_file_is_an_error_not_a_silent_none() {
        let dir = temp_dir("damaged");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(FILE_NAME), "{ not json").unwrap();
        let err = TokenStore::new(&dir).load().unwrap_err();
        assert!(
            err.to_string().starts_with("the token file is not valid"),
            "{err}"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_state_directory_is_chosen_in_a_fixed_order() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(
            state_dir(Some("/x"), env(&[("HOME", "/h")])),
            PathBuf::from("/x")
        );
        assert_eq!(
            state_dir(None, env(&[("PYXEETB_STATE_DIR", "/s"), ("HOME", "/h")])),
            PathBuf::from("/s")
        );
        assert_eq!(
            state_dir(None, env(&[("XDG_CONFIG_HOME", "/c"), ("HOME", "/h")])),
            Path::new("/c").join("pyxeetb")
        );
        assert_eq!(
            state_dir(None, env(&[("HOME", "/h")])),
            Path::new("/h").join(".config").join("pyxeetb")
        );
        assert_eq!(state_dir(None, env(&[])), PathBuf::from("pyxeetb-state"));
        assert_eq!(
            state_dir(Some(""), env(&[("HOME", "/h")])),
            Path::new("/h").join(".config").join("pyxeetb")
        );
    }
}
