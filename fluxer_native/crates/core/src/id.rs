use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::{fmt, str::FromStr};

/// A lossless numeric identifier. Fluxer transmits snowflakes as decimal JSON
/// strings: accepting JSON numbers could silently import rounded JS values.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Snowflake(pub u64);
impl Snowflake {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u64 {
        self.0
    }
}
impl From<u64> for Snowflake {
    fn from(value: u64) -> Self {
        Self(value)
    }
}
impl fmt::Display for Snowflake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl FromStr for Snowflake {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() || !value.bytes().all(|v| v.is_ascii_digit()) {
            return Err("snowflake must be an unsigned decimal string");
        }
        value.parse().map(Self).map_err(|_| "snowflake exceeds u64")
    }
}
impl Serialize for Snowflake {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}
impl<'de> Deserialize<'de> for Snowflake {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_id_is_lossless_and_strict() {
        let id: Snowflake = serde_json::from_str("\"18446744073709551615\"").unwrap();
        assert_eq!(id.0, u64::MAX);
        assert_eq!(
            serde_json::to_string(&id).unwrap(),
            "\"18446744073709551615\""
        );
        for value in [
            "9007199254740993",
            "\"-1\"",
            "\"+1\"",
            "\"\"",
            "\"18446744073709551616\"",
        ] {
            assert!(serde_json::from_str::<Snowflake>(value).is_err(), "{value}");
        }
        assert!(Snowflake(10) > Snowflake(9));
    }
}
