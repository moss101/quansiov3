//! Remote control of a person's own computer (ADP-009, DOMAIN.md §8.6.2).
//!
//! A remote session is **one viewer on one native computer**, and it exists only because the person at that computer said yes to that
//! viewer. This module is the pure half of it, shared by the two places that must agree: the server relay (which decides what is
//! forwarded) and the host component (which decides what is injected into the machine). Neither trusts the other's word:
//!
//! * [`session`] is the authorization state machine: `Requested → Authorized → Stopped`, with every transition a function of the
//!   caller's identity, the clock and (for input and frames) the computer's own report of whether it is capturing, unlocked and
//!   permitted. Nothing is queued; a message that is not admitted now is refused with a typed [`session::Refusal`].
//! * [`protocol`] is the wire form: what a viewer may send (pointer, scroll, key and text events with bounds) and what a computer may
//!   send (frames and its readiness report), parsed strictly — an unknown field, an over-large frame, a coordinate off the screen or a
//!   key that is not a key is a refusal before any state is consulted.
//!
//! There is no I/O here and no clock: callers pass `now_ms`.

pub mod protocol;
pub mod session;

use serde::Deserialize;

const SHIPPED: &str = include_str!("../../../../config/remote-control.yaml");

/// `config/remote-control.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteConfig {
    /// Always 1.
    pub version: u32,
    /// Off: a request is refused.
    pub enabled: bool,
    /// How long a request waits for the person at the computer.
    pub consent_ttl_seconds: i64,
    /// The longest a person may grant.
    pub grant_max_seconds: i64,
    /// The most any session lasts.
    pub session_max_seconds: i64,
    /// How long a control session may sit with no viewer activity.
    pub session_idle_seconds: i64,
    /// How fresh the computer's readiness report must be for input to be admitted.
    pub host_report_max_age_seconds: i64,
    /// The largest frame (bytes of the base64 text).
    pub frame_max_bytes: usize,
    /// Frames per second.
    pub frame_max_per_second: u32,
    /// Input events per second.
    pub input_max_per_second: u32,
    /// The longest typed text, in characters.
    pub text_max_chars: usize,
    /// The largest pointer coordinate.
    pub coordinate_max: u32,
    /// The most bytes one clipboard transfer carries (text bytes, or the decoded bytes of all its files together).
    pub clipboard_max_bytes: usize,
    /// The most files one clipboard transfer carries.
    pub clipboard_max_files: usize,
    /// Clipboard transfers per second.
    pub clipboard_max_per_second: u32,
}

impl RemoteConfig {
    /// The shipped configuration.
    ///
    /// # Panics
    /// Panics only if the repository's own `config/remote-control.yaml` is invalid, which its tests catch first.
    #[must_use]
    pub fn builtin() -> Self {
        Self::from_yaml(SHIPPED).expect("config/remote-control.yaml is valid")
    }

    /// Parse and validate.
    ///
    /// # Errors
    /// Returns the rule that is broken.
    pub fn from_yaml(text: &str) -> Result<Self, String> {
        let config: Self = serde_yaml::from_str(text).map_err(|error| error.to_string())?;
        if config.version != 1 {
            return Err("version must be 1".to_string());
        }
        let ranges = [
            ("consent_ttl_seconds", config.consent_ttl_seconds, 10, 600),
            ("grant_max_seconds", config.grant_max_seconds, 60, 14_400),
            (
                "session_max_seconds",
                config.session_max_seconds,
                60,
                14_400,
            ),
            (
                "session_idle_seconds",
                config.session_idle_seconds,
                30,
                3_600,
            ),
            (
                "host_report_max_age_seconds",
                config.host_report_max_age_seconds,
                2,
                120,
            ),
        ];
        for (name, value, low, high) in ranges {
            if value < low || value > high {
                return Err(format!("{name} is {low} to {high}"));
            }
        }
        if config.frame_max_bytes < 1024
            || config.frame_max_bytes > 4_194_304
            || !(1..=60).contains(&config.frame_max_per_second)
            || !(1..=240).contains(&config.input_max_per_second)
            || !(1..=4096).contains(&config.text_max_chars)
            || !(640..=65_536).contains(&config.coordinate_max)
        {
            return Err("a frame, rate, text or coordinate bound is outside its range".to_string());
        }
        if !(1..=1_048_576).contains(&config.clipboard_max_bytes)
            || config.clipboard_max_files > 8
            || !(1..=10).contains(&config.clipboard_max_per_second)
        {
            return Err("a clipboard bound is outside its range".to_string());
        }
        // A transfer travels as one text message, base64 inside JSON: it must fit the message bound a frame has.
        if config.clipboard_max_bytes.div_ceil(3) * 4 + 1024 > config.frame_max_bytes {
            return Err(
                "clipboard_max_bytes must fit inside frame_max_bytes once encoded".to_string(),
            );
        }
        if config.grant_max_seconds > config.session_max_seconds {
            return Err("grant_max_seconds cannot pass session_max_seconds".to_string());
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_configuration_is_valid_and_bounded() {
        let config = RemoteConfig::builtin();
        assert!(config.enabled);
        assert_eq!(config.consent_ttl_seconds, 120);
        assert!(config.grant_max_seconds <= config.session_max_seconds);
        for (from, to) in [
            ("consent_ttl_seconds: 120", "consent_ttl_seconds: 6000"),
            ("consent_ttl_seconds: 120", "consent_ttl_seconds: 1"),
            ("session_max_seconds: 3600", "session_max_seconds: 999999"),
            ("grant_max_seconds: 3600", "grant_max_seconds: 7200"),
            ("input_max_per_second: 60", "input_max_per_second: 0"),
            ("frame_max_bytes: 262144", "frame_max_bytes: 99999999"),
            ("coordinate_max: 16384", "coordinate_max: 10"),
            ("enabled: true", "enabled: maybe"),
            ("clipboard_max_bytes: 65536", "clipboard_max_bytes: 262144"),
            ("clipboard_max_files: 2", "clipboard_max_files: 20"),
            ("clipboard_max_per_second: 2", "clipboard_max_per_second: 0"),
        ] {
            assert!(
                RemoteConfig::from_yaml(&SHIPPED.replace(from, to)).is_err(),
                "{to}"
            );
        }
        assert!(
            RemoteConfig::from_yaml(&format!("{SHIPPED}\nviewer_can_skip_consent: true\n"))
                .is_err()
        );
    }
}
