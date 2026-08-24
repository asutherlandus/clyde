//! Human-readable durations at config and API boundaries.
//!
//! Durations are serialised as strings (`"45m"`, `"10s"`) and held as
//! [`std::time::Duration`] internally (schema reference: Time).

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::ValidationError;

/// A positive duration with a stable string encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HumanDuration(Duration);

const SECOND: u64 = 1;
const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

impl HumanDuration {
    /// Parses `<positive integer><unit>` where unit is `s`, `m`, `h`, or `d`.
    pub fn parse(value: &str) -> Result<Self, ValidationError> {
        let malformed = || ValidationError::MalformedDuration {
            value: value.to_owned(),
        };
        let trimmed = value.trim();
        let split = trimmed
            .char_indices()
            .find(|(_, c)| !c.is_ascii_digit())
            .map(|(index, _)| index)
            .ok_or_else(malformed)?;
        let (digits, unit) = trimmed.split_at(split);
        if digits.is_empty() {
            return Err(malformed());
        }
        let magnitude: u64 = digits.parse().map_err(|_| malformed())?;
        let seconds_per_unit = match unit {
            "s" => SECOND,
            "m" => MINUTE,
            "h" => HOUR,
            "d" => DAY,
            _ => return Err(malformed()),
        };
        let seconds = magnitude
            .checked_mul(seconds_per_unit)
            .ok_or_else(malformed)?;
        if seconds == 0 {
            return Err(ValidationError::ZeroDuration);
        }
        Ok(Self(Duration::from_secs(seconds)))
    }

    /// Wraps a duration, rejecting zero.
    pub fn from_duration(duration: Duration) -> Result<Self, ValidationError> {
        if duration.as_secs() == 0 {
            return Err(ValidationError::ZeroDuration);
        }
        Ok(Self(duration))
    }

    pub fn as_duration(self) -> Duration {
        self.0
    }

    pub fn as_secs(self) -> u64 {
        self.0.as_secs()
    }

    /// Renders in the largest unit that divides exactly, so a round-trip through
    /// the string form is stable.
    pub fn render(self) -> String {
        let secs = self.0.as_secs();
        for (unit_secs, suffix) in [(DAY, 'd'), (HOUR, 'h'), (MINUTE, 'm')] {
            if secs.is_multiple_of(unit_secs) {
                return format!("{}{suffix}", secs / unit_secs);
            }
        }
        format!("{secs}s")
    }

    /// The shorter of two durations. Used wherever a derived value must not
    /// exceed its parent (derivation rule 7).
    pub fn min(self, other: Self) -> Self {
        if self.0 <= other.0 { self } else { other }
    }
}

impl fmt::Display for HumanDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}

impl Serialize for HumanDuration {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.render())
    }
}

impl<'de> Deserialize<'de> for HumanDuration {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn parses_each_unit() {
        assert_eq!(HumanDuration::parse("10s").unwrap().as_secs(), 10);
        assert_eq!(HumanDuration::parse("45m").unwrap().as_secs(), 2700);
        assert_eq!(HumanDuration::parse("2h").unwrap().as_secs(), 7200);
        assert_eq!(HumanDuration::parse("1d").unwrap().as_secs(), 86400);
        assert_eq!(HumanDuration::parse(" 30m ").unwrap().as_secs(), 1800);
    }

    #[test]
    fn rejects_malformed_and_zero() {
        for bad in [
            "",
            "m",
            "10",
            "10x",
            "-5m",
            "1.5h",
            "10 m",
            "999999999999999999999s",
        ] {
            assert!(HumanDuration::parse(bad).is_err(), "{bad} must be rejected");
        }
        assert_eq!(
            HumanDuration::parse("0s"),
            Err(ValidationError::ZeroDuration)
        );
    }

    #[test]
    fn render_round_trips() {
        for text in ["10s", "45m", "2h", "3d", "90s"] {
            let parsed = HumanDuration::parse(text).unwrap();
            let rendered = parsed.render();
            assert_eq!(HumanDuration::parse(&rendered).unwrap(), parsed);
        }
        assert_eq!(HumanDuration::parse("60s").unwrap().render(), "1m");
        assert_eq!(HumanDuration::parse("90s").unwrap().render(), "90s");
    }

    #[test]
    fn serde_uses_the_string_form() {
        let value = HumanDuration::parse("45m").unwrap();
        assert_eq!(serde_json::to_string(&value).unwrap(), "\"45m\"");
        let back: HumanDuration = serde_json::from_str("\"45m\"").unwrap();
        assert_eq!(back, value);
        assert!(serde_json::from_str::<HumanDuration>("\"nope\"").is_err());
        assert!(serde_json::from_str::<HumanDuration>("2700").is_err());
    }

    #[test]
    fn min_picks_the_shorter() {
        let a = HumanDuration::parse("1h").unwrap();
        let b = HumanDuration::parse("30m").unwrap();
        assert_eq!(a.min(b), b);
        assert_eq!(b.min(a), b);
    }
}
