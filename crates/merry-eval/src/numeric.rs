use schemars::{Schema, SchemaGenerator, json_schema};
use serde::de::{self, Deserializer, MapAccess, Visitor};
use std::fmt;

const U64_EXCLUSIVE_UPPER_BOUND: f64 = 18_446_744_073_709_551_616.0;
const SERDE_JSON_NUMBER_KEY: &str = "$serde_json::private::Number";

pub(crate) fn nonnegative_u64_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "number",
        "minimum": 0,
        "maximum": u64::MAX,
        "multipleOf": 1,
    })
}

pub(crate) fn optional_nonnegative_u64_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": ["number", "null"],
        "minimum": 0,
        "maximum": u64::MAX,
        "multipleOf": 1,
    })
}

pub(crate) fn positive_u32_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "number",
        "minimum": 1,
        "maximum": u32::MAX,
        "multipleOf": 1,
    })
}

pub(crate) fn optional_positive_u32_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": ["number", "null"],
        "minimum": 1,
        "maximum": u32::MAX,
        "multipleOf": 1,
    })
}

pub(crate) fn optional_positive_u64_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": ["number", "null"],
        "minimum": 1,
        "maximum": u64::MAX,
        "multipleOf": 1,
    })
}

pub(crate) fn task_timeout_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "number",
        "minimum": 1,
        "maximum": 604800,
        "multipleOf": 1,
    })
}

pub(crate) fn version_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "number",
        "const": 1,
        "multipleOf": 1,
    })
}

fn parse_integral_decimal(raw: &str) -> Result<u64, &'static str> {
    let (negative, raw) = raw
        .strip_prefix('-')
        .map_or((false, raw), |rest| (true, rest));
    let (mantissa, exponent) = raw
        .split_once(['e', 'E'])
        .map_or((raw, 0_i64), |(mantissa, exponent)| {
            (mantissa, exponent.parse::<i64>().unwrap_or(i64::MIN))
        });
    if exponent == i64::MIN || mantissa.is_empty() {
        return Err("number has an invalid exponent");
    }

    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty() || !whole.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("number has an invalid integer part");
    }
    if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("number has an invalid fractional part");
    }

    let digits = format!("{whole}{fraction}");
    let first_nonzero = digits.bytes().position(|byte| byte != b'0');
    let Some(first_nonzero) = first_nonzero else {
        return Ok(0);
    };
    if negative {
        return Err("number must not be negative");
    }

    let scale = exponent
        .checked_sub(i64::try_from(fraction.len()).map_err(|_| "fraction is too long")?)
        .ok_or("number exponent underflow")?;
    if scale < 0 {
        let remove = usize::try_from(scale.unsigned_abs()).map_err(|_| "number is too small")?;
        if remove >= digits.len() {
            return Err("number must be an integer");
        }
        let split = digits.len() - remove;
        if digits[split..].bytes().any(|byte| byte != b'0') {
            return Err("number must be an integer");
        }
        return parse_u64_digits(&digits[..split]);
    }

    let significant = &digits[first_nonzero..];
    let zeros = usize::try_from(scale).map_err(|_| "number exceeds u64 range")?;
    if significant.len().saturating_add(zeros) > 20 {
        return Err("number exceeds u64 range");
    }
    let mut value = parse_u64_digits(significant)?;
    for _ in 0..zeros {
        value = value.checked_mul(10).ok_or("number exceeds u64 range")?;
    }
    Ok(value)
}

fn parse_u64_digits(digits: &str) -> Result<u64, &'static str> {
    digits.bytes().try_fold(0_u64, |value, byte| {
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(byte - b'0')))
            .ok_or("number exceeds u64 range")
    })
}

struct IntegralNumberVisitor;

impl<'de> Visitor<'de> for IntegralNumberVisitor {
    type Value = u64;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a non-negative integer or an integral JSON number")
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(value)
    }

    fn visit_u128<E>(self, value: u128) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        u64::try_from(value).map_err(|_| E::custom("integer exceeds u64 range"))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        u64::try_from(value).map_err(|_| E::custom("integer must not be negative"))
    }

    fn visit_i128<E>(self, value: i128) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        u64::try_from(value).map_err(|_| E::custom("integer must be non-negative and fit u64"))
    }

    fn visit_f32<E>(self, value: f32) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.visit_f64(f64::from(value))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
            return Err(E::custom("number must be a finite non-negative integer"));
        }
        if value >= U64_EXCLUSIVE_UPPER_BOUND {
            return Err(E::custom("number exceeds u64 range"));
        }
        Ok(value as u64)
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let Some(key) = map.next_key::<String>()? else {
            return Err(de::Error::custom("number is missing its value"));
        };
        if key != SERDE_JSON_NUMBER_KEY {
            return Err(de::Error::custom("unexpected map while parsing a number"));
        }
        let raw = map.next_value::<String>()?;
        if map.next_key::<String>()?.is_some() {
            return Err(de::Error::custom("number has multiple values"));
        }
        parse_integral_decimal(&raw).map_err(de::Error::custom)
    }
}

pub(crate) fn deserialize_u64<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_any(IntegralNumberVisitor)
}

pub(crate) fn deserialize_u32<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let value = deserialize_u64(deserializer)?;
    u32::try_from(value).map_err(|_| de::Error::custom("integer exceeds u32 range"))
}

struct OptionalIntegralNumberVisitor;

impl<'de> Visitor<'de> for OptionalIntegralNumberVisitor {
    type Value = Option<u64>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("null or a non-negative integer")
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_u64(deserializer).map(Some)
    }
}

pub(crate) fn deserialize_option_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_option(OptionalIntegralNumberVisitor)
}

pub(crate) fn deserialize_option_u32<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_option_u64(deserializer)?
        .map(u32::try_from)
        .transpose()
        .map_err(|_| de::Error::custom("integer exceeds u32 range"))
}
