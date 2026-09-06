//! Shared serde adapters for runtime-owned provider tool inputs.

use serde::{Deserializer, de};
use std::fmt;

pub(crate) fn deserialize_non_empty_process_command<'de, D>(
    deserializer: D,
) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    struct ProcessCommandVisitor;

    impl<'de> de::Visitor<'de> for ProcessCommandVisitor {
        type Value = String;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a non-empty shell command string")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            if value.is_empty() {
                return Err(E::custom("command must be a non-empty string"));
            }
            Ok(value.to_owned())
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            self.visit_str(&value)
        }

        fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("command must be a non-empty string"))
        }

        fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("command must be a non-empty string"))
        }

        fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("command must be a non-empty string"))
        }

        fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("command must be a non-empty string"))
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("command must be a non-empty string"))
        }

        fn visit_seq<A>(self, _sequence: A) -> Result<Self::Value, A::Error>
        where
            A: de::SeqAccess<'de>,
        {
            Err(de::Error::custom("command must be a non-empty string"))
        }

        fn visit_map<A>(self, _map: A) -> Result<Self::Value, A::Error>
        where
            A: de::MapAccess<'de>,
        {
            Err(de::Error::custom("command must be a non-empty string"))
        }
    }

    deserializer.deserialize_any(ProcessCommandVisitor)
}
