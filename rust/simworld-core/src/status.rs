//! Data-source health for the host (contract `status()` / `status_changed`, api 0.7).
//!
//! Plain data derived on the main thread from the worker results `World` already drains:
//! fetch outcomes (with their [`FetchErrorKind`]), decode failures and configuration errors.
//! No timers and no threads; the time window is evaluated against the `now` passed in.
//!
//! Rules, first match wins:
//! 1. **error**: a configuration problem that retrying cannot fix (the archive does not match
//!    the tileset contract; the workers could not start). Sticky until the next configure.
//! 2. **offline**: the archive cannot be opened (latest network outcome) and no tile has been
//!    served at all in this configuration, neither from the network nor from the disk cache.
//! 3. **degraded**: the archive cannot be opened but cached tiles are being served, or the disk
//!    cache was disabled for the session (repeated write failures), or any fetch/decode failure
//!    happened within the hold window (`streaming.retry_max_s` + 5 s, so a tile that keeps
//!    failing at the maximum backoff keeps the state steady).
//! 4. **ok** otherwise (always for the synthetic city).
//!
//! The message is stable per state (details go to `last_error`), so `status_changed` fires
//! only on real transitions, not per failed tile.

use std::time::{Duration, Instant};

use crate::fetch::{FetchError, FetchErrorKind, Source};

/// Status state (contract strings `ok`, `degraded`, `offline`, `error`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusState {
    Ok,
    Degraded,
    Offline,
    Error,
}

impl StatusState {
    pub fn as_str(self) -> &'static str {
        match self {
            StatusState::Ok => "ok",
            StatusState::Degraded => "degraded",
            StatusState::Offline => "offline",
            StatusState::Error => "error",
        }
    }
}

/// Contract `status()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub state: StatusState,
    /// Human-readable summary, stable per state (empty when ok and configured).
    pub message: String,
    /// Failed tile fetches since the last configure (cumulative; retries count again).
    pub failed_fetches: u64,
    /// The latest fetch or decode error (empty if none since the last configure).
    pub last_error: String,
    /// The terrain is active in this configuration (api 1.6): the host asked for the layer and
    /// the profile enables it. False when not configured or after a failed configure.
    pub terrain_active: bool,
}

pub const MSG_NOT_CONFIGURED: &str = "not configured";
pub const MSG_SHUT_DOWN: &str = "shut down";
pub const MSG_OFFLINE: &str = "tile server unreachable and no cached tiles";
pub const MSG_CACHED_ONLY: &str = "tile server unreachable; showing cached tiles only";
pub const MSG_FAILURES: &str = "some tiles failed to load; retrying";
pub const MSG_CACHE_DISABLED: &str = "disk cache disabled after write failures; streaming without it";

/// Accumulates outcomes for one configuration.
#[derive(Clone, Debug)]
pub struct Health {
    /// Message while no world is running (`not configured`, `shut down`); `None` = running.
    idle: Option<&'static str>,
    fatal: Option<String>,
    failed_fetches: u64,
    last_error: String,
    /// Tiles served (network or disk) in this configuration.
    served: u64,
    /// The latest archive access failed to open it; cleared by a network success.
    unreachable: bool,
    last_failure: Option<Instant>,
    hold: Duration,
    /// The disk cache was disabled for the session (sticky until the next configure).
    cache_disabled: bool,
}

impl Health {
    /// Health of a world that is not running (`idle` = the message).
    pub fn idle(msg: &'static str) -> Self {
        Self {
            idle: Some(msg),
            fatal: None,
            failed_fetches: 0,
            last_error: String::new(),
            served: 0,
            unreachable: false,
            last_failure: None,
            hold: Duration::ZERO,
            cache_disabled: false,
        }
    }

    /// Fresh health of a running configuration; failures keep `degraded` for `hold`.
    pub fn running(hold: Duration) -> Self {
        Self { idle: None, hold, ..Self::idle("") }
    }

    /// A configuration error that retrying will not fix.
    pub fn set_fatal(&mut self, msg: &str) {
        if self.fatal.is_none() {
            self.fatal = Some(msg.to_string());
        }
        self.last_error = msg.to_string();
    }

    pub fn record_fetch(&mut self, result: &Result<(bytes::Bytes, Source), FetchError>, now: Instant) {
        match result {
            Ok((_, source)) => {
                self.served += 1;
                if *source == Source::Network {
                    self.unreachable = false;
                }
            }
            // A fetch the scheduler cancelled says nothing about the source.
            Err(e) if e.kind == FetchErrorKind::Cancelled => {}
            Err(e) => {
                self.failed_fetches += 1;
                self.last_error = e.message.clone();
                self.last_failure = Some(now);
                match e.kind {
                    FetchErrorKind::Unreachable => self.unreachable = true,
                    FetchErrorKind::Contract => self.set_fatal(&e.message),
                    FetchErrorKind::Tile | FetchErrorKind::Cancelled => {}
                }
            }
        }
    }

    /// The disk cache was disabled for this configuration (details in `last_error`).
    pub fn set_cache_disabled(&mut self, msg: &str) {
        self.cache_disabled = true;
        self.last_error = msg.to_string();
    }

    pub fn record_decode_failure(&mut self, msg: &str, now: Instant) {
        self.last_error = msg.to_string();
        self.last_failure = Some(now);
    }

    pub fn status_at(&self, now: Instant) -> Status {
        let (state, message) = if let Some(m) = self.idle {
            (StatusState::Ok, m.to_string())
        } else if let Some(f) = &self.fatal {
            (StatusState::Error, f.clone())
        } else if self.unreachable && self.served == 0 {
            (StatusState::Offline, MSG_OFFLINE.to_string())
        } else if self.unreachable {
            (StatusState::Degraded, MSG_CACHED_ONLY.to_string())
        } else if self.cache_disabled {
            (StatusState::Degraded, MSG_CACHE_DISABLED.to_string())
        } else if self.last_failure.is_some_and(|t| now.saturating_duration_since(t) < self.hold) {
            (StatusState::Degraded, MSG_FAILURES.to_string())
        } else {
            (StatusState::Ok, String::new())
        };
        Status { state, message, failed_fetches: self.failed_fetches, last_error: self.last_error.clone(), terrain_active: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use FetchErrorKind::*;

    fn err(kind: FetchErrorKind, m: &str) -> Result<(Bytes, Source), FetchError> {
        Err(FetchError::new(kind, m))
    }
    fn ok(s: Source) -> Result<(Bytes, Source), FetchError> {
        Ok((Bytes::new(), s))
    }

    #[test]
    fn idle_states() {
        let t = Instant::now();
        let s = Health::idle(MSG_NOT_CONFIGURED).status_at(t);
        assert_eq!((s.state, s.message.as_str(), s.failed_fetches), (StatusState::Ok, MSG_NOT_CONFIGURED, 0));
        assert_eq!(Health::idle(MSG_SHUT_DOWN).status_at(t).message, MSG_SHUT_DOWN);
        let s = Health::running(Duration::from_secs(35)).status_at(t);
        assert_eq!((s.state, s.message.as_str(), s.last_error.as_str()), (StatusState::Ok, "", ""));
    }

    #[test]
    fn unreachable_server_is_offline_until_something_is_served() {
        let t0 = Instant::now();
        let mut h = Health::running(Duration::from_secs(35));
        h.record_fetch(&err(Unreachable, "open http://x: connection refused"), t0);
        h.record_fetch(&err(Unreachable, "open http://x: connection refused"), t0);
        let s = h.status_at(t0);
        assert_eq!((s.state, s.message.as_str()), (StatusState::Offline, MSG_OFFLINE));
        assert_eq!((s.failed_fetches, s.last_error.as_str()), (2, "open http://x: connection refused"));
        // Stays offline however long it lasts (no time window).
        assert_eq!(h.status_at(t0 + Duration::from_secs(3600)).state, StatusState::Offline);
        // A warm-cache hit: degraded, cached tiles only (disk hits do not clear "unreachable").
        h.record_fetch(&ok(Source::Disk), t0);
        let s = h.status_at(t0);
        assert_eq!((s.state, s.message.as_str()), (StatusState::Degraded, MSG_CACHED_ONLY));
        h.record_fetch(&ok(Source::Disk), t0 + Duration::from_secs(100));
        assert_eq!(h.status_at(t0 + Duration::from_secs(100)).message, MSG_CACHED_ONLY);
        // The server answers: failures in the window keep it degraded, then ok.
        let t1 = t0 + Duration::from_secs(10);
        h.record_fetch(&ok(Source::Network), t1);
        let s = h.status_at(t1);
        assert_eq!((s.state, s.message.as_str()), (StatusState::Degraded, MSG_FAILURES));
        assert_eq!(h.status_at(t0 + Duration::from_secs(35)).state, StatusState::Ok);
        assert_eq!(h.status_at(t0 + Duration::from_secs(35)).failed_fetches, 2, "cumulative");
    }

    #[test]
    fn tile_failures_degrade_for_the_hold_window() {
        let t0 = Instant::now();
        let mut h = Health::running(Duration::from_secs(35));
        h.record_fetch(&ok(Source::Network), t0);
        assert_eq!(h.status_at(t0).state, StatusState::Ok);
        h.record_fetch(&err(Tile, "16/1/2: timeout"), t0);
        assert_eq!(h.status_at(t0 + Duration::from_secs(34)).state, StatusState::Degraded);
        assert_eq!(h.status_at(t0 + Duration::from_secs(35)).state, StatusState::Ok);
        h.record_decode_failure("decode panic: x", t0 + Duration::from_secs(40));
        let s = h.status_at(t0 + Duration::from_secs(41));
        assert_eq!((s.state, s.failed_fetches, s.last_error.as_str()), (StatusState::Degraded, 1, "decode panic: x"));
    }

    #[test]
    fn disabled_disk_cache_degrades_until_reconfigured() {
        let t0 = Instant::now();
        let mut h = Health::running(Duration::from_secs(35));
        h.record_fetch(&ok(Source::Network), t0);
        h.set_cache_disabled("disk cache x disabled for this session after 3 failed writes: disk full");
        let s = h.status_at(t0 + Duration::from_secs(3600));
        assert_eq!((s.state, s.message.as_str()), (StatusState::Degraded, MSG_CACHE_DISABLED));
        assert!(s.last_error.contains("disk full"));
        assert_eq!(Health::running(Duration::from_secs(35)).status_at(t0).state, StatusState::Ok);
    }

    #[test]
    fn contract_mismatch_is_a_sticky_error() {
        let t0 = Instant::now();
        let mut h = Health::running(Duration::from_secs(35));
        h.record_fetch(&err(Contract, "x.pmtiles: tileset lacks required layer(s) roads"), t0);
        h.record_fetch(&ok(Source::Disk), t0);
        let s = h.status_at(t0 + Duration::from_secs(3600));
        assert_eq!(s.state, StatusState::Error);
        assert_eq!(s.message, "x.pmtiles: tileset lacks required layer(s) roads");
        // The first fatal message stays; later errors only update last_error.
        h.set_fatal("other");
        assert_eq!(h.status_at(t0).message, "x.pmtiles: tileset lacks required layer(s) roads");
        assert_eq!(h.status_at(t0).last_error, "other");
    }
}
