//! Check for Updates: the latest release on GitHub against the running version, asked on
//! a thread at startup (at most once a day, unless turned off in Settings) and from the
//! menu.

use std::fmt;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use eframe::Storage;
use jiff::{SignedDuration, Timestamp};
use semver::Version;

/// The repository whose releases are Kymo's.
const REPOSITORY: &str = "cbrnr/kymo";

const CHECKED_KEY: &str = "kymo.update_checked";
const SKIPPED_KEY: &str = "kymo.update_skipped";

/// How long a startup check counts: no other one starts before.
const INTERVAL: SignedDuration = SignedDuration::from_hours(24);

/// A published release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// Its tag, `v` and the version as a rule.
    tag: String,
}

impl Release {
    /// The release with `tag`, `v` and a version (or the version alone); `None` for any
    /// other tag.
    pub fn from_tag(tag: &str) -> Option<Release> {
        let version = Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()?;
        Some(Release {
            version,
            tag: tag.to_owned(),
        })
    }

    /// Its page on GitHub, with the notes and the downloads.
    pub fn url(&self) -> String {
        format!("https://github.com/{REPOSITORY}/releases/tag/{}", self.tag)
    }

    /// Whether it is newer than the running version (a release is newer than its
    /// candidates).
    pub fn is_newer(&self) -> bool {
        self.version > current()
    }
}

/// The running version.
pub fn current() -> Version {
    // Cargo checks that the manifest's version is one.
    Version::parse(kymo_core::version()).unwrap_or_else(|_| Version::new(0, 0, 0))
}

/// The latest release from GitHub's answer for it (its API's `releases/latest`).
pub fn parse_latest(json: &str) -> Result<Release> {
    let value: serde_json::Value = serde_json::from_str(json).context("unreadable answer")?;
    let tag = value
        .get("tag_name")
        .and_then(|t| t.as_str())
        .ok_or_else(|| anyhow!("the answer names no release"))?;
    Release::from_tag(tag).ok_or_else(|| anyhow!("the latest release, {tag}, has no version"))
}

/// Asks GitHub for the latest release; `None` before the first one. Drafts and
/// pre-releases don't count.
pub fn fetch_latest() -> Result<Option<Release>> {
    // Mozilla's root certificates, built in: macOS's own verifier (rustls-platform-verifier)
    // hung until the timeout.
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .user_agent(format!("kymo/{}", kymo_core::version()))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases/latest");
    let answer = agent
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call();
    let mut answer = match answer {
        Ok(answer) => answer,
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(ureq::Error::StatusCode(status)) => {
            return Err(anyhow!("GitHub answered with status {status}"));
        }
        Err(error) => return Err(anyhow!("cannot reach GitHub: {error}")),
    };
    let json = answer
        .body_mut()
        .read_to_string()
        .context("cannot read GitHub's answer")?;
    parse_latest(&json).map(Some)
}

/// Where a check asks for the latest release: GitHub, or a stand-in for tests.
#[derive(Clone)]
pub struct Source(Arc<dyn Fn() -> Result<Option<Release>> + Send + Sync>);

impl Source {
    /// GitHub's latest release ([`fetch_latest`]).
    pub fn github() -> Source {
        Source::new(fetch_latest)
    }

    pub fn new(fetch: impl Fn() -> Result<Option<Release>> + Send + Sync + 'static) -> Source {
        Source(Arc::new(fetch))
    }
}

impl fmt::Debug for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Source")
    }
}

/// A check under way on a thread.
pub struct Check {
    result: Receiver<Result<Option<Release>>>,
}

impl Check {
    /// Starts asking `source`; `notify` runs once the answer is there, so the window can
    /// repaint and pick it up with [`poll`](Self::poll).
    pub fn start(source: &Source, notify: impl FnOnce() + Send + 'static) -> Check {
        let (tx, rx) = mpsc::channel();
        let fetch = source.0.clone();
        // If spawning fails, `tx` is dropped with the closure and `poll` reports it.
        let _ = std::thread::Builder::new()
            .name("kymo-update".into())
            .spawn(move || {
                let _ = tx.send(fetch());
                notify();
            });
        Check { result: rx }
    }

    /// The answer, once it is there.
    pub fn poll(&self) -> Option<Result<Option<Release>>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(anyhow!("the check failed unexpectedly"))),
        }
    }
}

/// What is remembered between runs: when the startup check last ran, and the version
/// whose offer was skipped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdateState {
    pub last_check: Option<Timestamp>,
    pub skipped: Option<Version>,
}

impl UpdateState {
    /// Whether a startup check is due at `now`: none ran within a day (or the last one is
    /// in the future, the clock having been set back).
    pub fn due(&self, now: Timestamp) -> bool {
        self.last_check
            .is_none_or(|last| last > now || now.duration_since(last) >= INTERVAL)
    }

    /// Whether the startup check offers `release`: it is newer, and not skipped.
    pub fn offers(&self, release: &Release) -> bool {
        release.is_newer() && self.skipped.as_ref() != Some(&release.version)
    }

    /// The state kept in `storage`; what is missing or unreadable is `None`.
    pub fn load(storage: Option<&dyn Storage>) -> UpdateState {
        let get = |key: &str| storage.and_then(|s| s.get_string(key));
        UpdateState {
            last_check: get(CHECKED_KEY)
                .and_then(|v| v.trim().parse().ok())
                .and_then(|s| Timestamp::from_second(s).ok()),
            skipped: get(SKIPPED_KEY).and_then(|v| Version::parse(v.trim()).ok()),
        }
    }

    pub fn save(&self, storage: &mut dyn Storage) {
        match self.last_check {
            Some(time) => storage.set_string(CHECKED_KEY, time.as_second().to_string()),
            None => storage.remove_string(CHECKED_KEY),
        }
        match &self.skipped {
            Some(version) => storage.set_string(SKIPPED_KEY, version.to_string()),
            None => storage.remove_string(SKIPPED_KEY),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_latest_release() {
        let json = r#"{"tag_name": "v0.2.0", "name": "0.2.0", "draft": false}"#;
        let release = parse_latest(json).unwrap();
        assert_eq!(release.version, Version::new(0, 2, 0));
        assert_eq!(
            release.url(),
            "https://github.com/cbrnr/kymo/releases/tag/v0.2.0"
        );
        assert!(parse_latest(r#"{"tag_name": "nightly"}"#).is_err());
        assert!(parse_latest(r#"{"message": "Not Found"}"#).is_err());
        assert!(parse_latest("<html>").is_err());
    }

    #[test]
    fn a_release_is_newer_than_its_candidates() {
        let release = Release::from_tag("v0.1.0").unwrap();
        assert!(release.version > Version::parse("0.1.0-rc.4").unwrap());
        assert!(
            Release::from_tag("0.1.0-rc.10").unwrap().version
                > Version::parse("0.1.0-rc.9").unwrap()
        );
        let far = Release::from_tag("v999.0.0").unwrap();
        assert!(far.is_newer());
        let running = Release::from_tag(kymo_core::version()).unwrap();
        assert!(!running.is_newer());
    }

    #[test]
    fn checks_at_most_once_a_day() {
        let now: Timestamp = "2026-11-23T12:00:00Z".parse().unwrap();
        assert!(UpdateState::default().due(now));
        let at = |t: &str| UpdateState {
            last_check: Some(t.parse().unwrap()),
            skipped: None,
        };
        assert!(!at("2026-11-23T01:00:00Z").due(now));
        assert!(at("2026-11-22T12:00:00Z").due(now));
        // The clock was set back.
        assert!(at("2027-01-01T00:00:00Z").due(now));
    }

    #[test]
    fn a_skipped_version_is_not_offered_again() {
        let newer = Release::from_tag("v999.0.0").unwrap();
        let mut state = UpdateState::default();
        assert!(state.offers(&newer));
        state.skipped = Some(newer.version.clone());
        assert!(!state.offers(&newer));
        assert!(state.offers(&Release::from_tag("v999.0.1").unwrap()));
        let running = Release::from_tag(kymo_core::version()).unwrap();
        assert!(!UpdateState::default().offers(&running));
    }
}
