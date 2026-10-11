//! Live reload: noticing that the configuration or the active theme changed.
//!
//! Files are polled rather than watched through the kernel: it works the same
//! on every filesystem, needs no extra dependency, and sees through the
//! symlinks that stow, chezmoi and home-manager put in place of the files.

use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

/// How often the files are looked at, in seconds.
pub const POLL_INTERVAL: f32 = 0.5;

/// What identifies a version of a file. The resolved path is part of it, so a
/// symlink that is re-pointed at an older file is noticed too.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Signature {
    resolved: PathBuf,
    modified: Option<SystemTime>,
    length: u64,
}

fn signature(path: &Path) -> Option<Signature> {
    // `metadata` follows symlinks, so this describes the file they lead to.
    let metadata = fs::metadata(path).ok()?;
    metadata.is_file().then_some(())?;
    Some(Signature {
        resolved: fs::canonicalize(path).ok()?,
        modified: metadata.modified().ok(),
        length: metadata.len(),
    })
}

/// One file under observation. A file that does not exist yet is watched too:
/// creating it counts as a change.
#[derive(Debug, Default)]
struct Tracked {
    path: Option<PathBuf>,
    last: Option<Signature>,
}

impl Tracked {
    fn new(path: Option<PathBuf>) -> Self {
        let last = path.as_deref().and_then(signature);
        Self { path, last }
    }

    /// Whether the file differs from the last time anyone looked.
    fn changed(&mut self) -> bool {
        let Some(path) = &self.path else {
            return false;
        };
        let now = signature(path);
        let changed = now != self.last;
        self.last = now;
        changed
    }
}

/// What changed since the last poll.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Changes {
    /// The configuration file: everything is reloaded.
    pub config: bool,
    /// The active theme file: only the colors are.
    pub theme: bool,
}

impl Changes {
    pub fn any(self) -> bool {
        self.config || self.theme
    }
}

#[derive(Debug, Default)]
pub struct Watcher {
    config: Tracked,
    theme: Tracked,
    elapsed: f32,
}

impl Watcher {
    pub fn new(config: Option<PathBuf>, theme: Option<PathBuf>) -> Self {
        Self {
            config: Tracked::new(config),
            theme: Tracked::new(theme),
            elapsed: 0.0,
        }
    }

    /// Follow a different theme file (or none), as of now.
    pub fn watch_theme(&mut self, theme: Option<PathBuf>) {
        self.theme = Tracked::new(theme);
    }

    /// Forget what was seen: whatever is on disk now is the baseline. Called
    /// after the program itself reloaded, so that it does not reload twice.
    pub fn rebase(&mut self) {
        self.config = Tracked::new(self.config.path.clone());
        self.theme = Tracked::new(self.theme.path.clone());
    }

    /// Account for `dt` seconds; every [`POLL_INTERVAL`] look at the files.
    pub fn poll(&mut self, dt: f32) -> Changes {
        if dt.is_finite() && dt > 0.0 {
            self.elapsed += dt;
        }
        if self.elapsed < POLL_INTERVAL {
            return Changes::default();
        }
        self.elapsed = 0.0;
        Changes {
            config: self.config.changed(),
            theme: self.theme.changed(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::{os::unix::fs::symlink, time::Duration};

    struct Directory(PathBuf);

    impl Directory {
        fn new(label: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("grava-watch-{}-{label}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Poll as the main loop would: a few frames that add up to the interval.
    fn poll(watcher: &mut Watcher) -> Changes {
        let mut changes = Changes::default();
        for _ in 0..31 {
            let seen = watcher.poll(1.0 / 60.0);
            changes.config |= seen.config;
            changes.theme |= seen.theme;
        }
        changes
    }

    fn set_modified(path: &Path, seconds_ago: u64) {
        let file = fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(seconds_ago))
            .unwrap();
    }

    #[test]
    fn nothing_is_reported_until_a_file_changes_and_then_once() {
        let directory = Directory::new("basic");
        let config = directory.path("config.toml");
        fs::write(&config, "[general]\n").unwrap();
        let mut watcher = Watcher::new(Some(config.clone()), None);
        assert!(!poll(&mut watcher).any());
        fs::write(&config, "[general]\nframerate = 30\n").unwrap();
        assert_eq!(
            poll(&mut watcher),
            Changes {
                config: true,
                theme: false
            }
        );
        assert!(!poll(&mut watcher).any(), "a change is reported once");
        // The same length and a new modification time still count.
        set_modified(&config, 100);
        assert!(poll(&mut watcher).config);
    }

    #[test]
    fn polling_waits_for_the_interval_and_ignores_bad_time_steps() {
        let directory = Directory::new("interval");
        let config = directory.path("config.toml");
        fs::write(&config, "a").unwrap();
        let mut watcher = Watcher::new(Some(config.clone()), None);
        fs::write(&config, "bb").unwrap();
        // A quarter second is too early; the change shows up by half a second.
        for _ in 0..15 {
            assert!(!watcher.poll(1.0 / 60.0).any());
        }
        assert!(
            (0..20).any(|_| watcher.poll(1.0 / 60.0).config),
            "reported after the interval"
        );
        for dt in [f32::NAN, -1.0, 0.0, f32::INFINITY] {
            let _ = watcher.poll(dt);
        }
    }

    #[test]
    fn creating_and_deleting_the_file_count_as_changes() {
        let directory = Directory::new("lifecycle");
        let config = directory.path("later.toml");
        let mut watcher = Watcher::new(Some(config.clone()), None);
        assert!(!poll(&mut watcher).any());
        fs::write(&config, "x").unwrap();
        assert!(poll(&mut watcher).config);
        fs::remove_file(&config).unwrap();
        assert!(poll(&mut watcher).config);
        assert!(!poll(&mut watcher).any());
        // Not watching anything is fine.
        assert!(!poll(&mut Watcher::default()).any());
    }

    #[test]
    fn symlinks_are_followed_and_a_repointed_link_is_a_change() {
        let directory = Directory::new("links");
        let first = directory.path("first.toml");
        let second = directory.path("second.toml");
        fs::write(&first, "same").unwrap();
        fs::write(&second, "same").unwrap();
        let link = directory.path("config.toml");
        symlink(&first, &link).unwrap();
        let mut watcher = Watcher::new(Some(link.clone()), None);
        assert!(!poll(&mut watcher).any());
        // Editing the target through the link is seen ...
        fs::write(&first, "edited").unwrap();
        assert!(poll(&mut watcher).config);
        // ... and so is a home-manager style swap to another file, even one
        // with the same contents and an older modification time.
        fs::write(&second, "edited").unwrap();
        set_modified(&second, 3600);
        fs::remove_file(&link).unwrap();
        symlink(&second, &link).unwrap();
        assert!(poll(&mut watcher).config);
        // A dangling link reads as a missing file.
        fs::remove_file(&second).unwrap();
        assert!(poll(&mut watcher).config);
    }

    #[test]
    fn the_theme_is_tracked_separately_and_can_be_switched_or_rebased() {
        let directory = Directory::new("theme");
        let config = directory.path("config.toml");
        let theme = directory.path("dune.toml");
        let other = directory.path("nord.toml");
        for file in [&config, &theme, &other] {
            fs::write(file, "1").unwrap();
        }
        let mut watcher = Watcher::new(Some(config.clone()), Some(theme.clone()));
        fs::write(&theme, "22").unwrap();
        assert_eq!(
            poll(&mut watcher),
            Changes {
                config: false,
                theme: true
            }
        );
        watcher.watch_theme(Some(other.clone()));
        fs::write(&theme, "333").unwrap();
        assert!(
            !poll(&mut watcher).any(),
            "the old theme is no longer watched"
        );
        fs::write(&other, "22").unwrap();
        assert!(poll(&mut watcher).theme);
        // After the program reloaded by itself, what is on disk is the baseline.
        fs::write(&config, "changed").unwrap();
        watcher.rebase();
        assert!(!poll(&mut watcher).any());
        watcher.watch_theme(None);
        fs::write(&other, "4444").unwrap();
        assert!(!poll(&mut watcher).any());
    }
}
