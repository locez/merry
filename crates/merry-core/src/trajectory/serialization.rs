//! Shared full-width counter decoding with legacy-number compatibility.

use super::TrajectoryTurnId;
use serde::{Deserialize, de};

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WireU64 {
    String(String),
    Number(u64),
}

impl WireU64 {
    fn parse<Error: de::Error>(self) -> Result<u64, Error> {
        match self {
            Self::String(value) => value.parse().map_err(de::Error::custom),
            Self::Number(value) => Ok(value),
        }
    }
}

pub(super) mod u64_as_string {
    use super::WireU64;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<Output>(value: &u64, serializer: Output) -> Result<Output::Ok, Output::Error>
    where
        Output: Serializer,
    {
        serializer.collect_str(value)
    }

    pub fn deserialize<'de, Input>(deserializer: Input) -> Result<u64, Input::Error>
    where
        Input: Deserializer<'de>,
    {
        WireU64::deserialize(deserializer)?.parse()
    }
}

pub(super) mod optional_u64_as_string {
    use super::WireU64;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<Output>(
        value: &Option<u64>,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error>
    where
        Output: Serializer,
    {
        match value {
            Some(value) => serializer.serialize_some(&value.to_string()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, Input>(deserializer: Input) -> Result<Option<u64>, Input::Error>
    where
        Input: Deserializer<'de>,
    {
        Option::<WireU64>::deserialize(deserializer)?
            .map(WireU64::parse)
            .transpose()
    }
}

pub(super) mod optional_turn_id_as_string {
    use super::{TrajectoryTurnId, WireU64};
    use serde::{Deserialize, Deserializer, Serializer, de};

    pub fn serialize<Output>(
        value: &Option<TrajectoryTurnId>,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error>
    where
        Output: Serializer,
    {
        match value {
            Some(value) => serializer.serialize_some(&value.value().to_string()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, Input>(
        deserializer: Input,
    ) -> Result<Option<TrajectoryTurnId>, Input::Error>
    where
        Input: Deserializer<'de>,
    {
        Option::<WireU64>::deserialize(deserializer)?
            .map(|value| TrajectoryTurnId::new(value.parse()?).map_err(de::Error::custom))
            .transpose()
    }
}
