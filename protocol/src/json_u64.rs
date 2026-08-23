//! Lossless JSON representation for counters that cross JavaScript's integer boundary.

use std::fmt;

use serde::{Deserializer, Serializer, de};

/// Largest integer that every JavaScript `number` can represent exactly.
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

pub fn serialize<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if *value <= MAX_SAFE_INTEGER {
        serializer.serialize_u64(*value)
    } else {
        serializer.serialize_str(&value.to_string())
    }
}

pub fn deserialize<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_any(U64Visitor)
}

struct U64Visitor;

impl de::Visitor<'_> for U64Visitor {
    type Value = u64;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a non-negative JSON integer or canonical decimal u64 string")
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(value)
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        u64::try_from(value).map_err(|_| E::custom("u64 value cannot be negative"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        parse_decimal(value).map_err(E::custom)
    }
}

fn parse_decimal(value: &str) -> Result<u64, &'static str> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err("expected a canonical decimal u64 string");
    }
    value.parse().map_err(|_| "decimal string exceeds u64")
}

pub mod option {
    use std::fmt;

    use serde::{Deserializer, Serializer, de};

    pub fn serialize<S>(value: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(value) => super::serialize(value, serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_option(OptionU64Visitor)
    }

    struct OptionU64Visitor;

    impl<'de> de::Visitor<'de> for OptionU64Visitor {
        type Value = Option<u64>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("null, a non-negative JSON integer, or a decimal u64 string")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            super::deserialize(deserializer).map(Some)
        }
    }
}
