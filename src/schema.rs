use regex::Regex;
use serde::{
    Deserialize, Serialize, Serializer,
    ser::{SerializeMap, SerializeSeq},
};
use serde_json::{Map as JsonMap, Number as JsonNumber, Value as JsonValue};

#[derive(Clone, Serialize, Debug)]
pub struct Config {
    #[serde(default)]
    pub props: Map,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Clone, Deserialize, Serialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Rule {
    Write {
        path: String,
        content: String,
        if_exists: IfExists,
    },
    Delete {
        path: String,
    },
    Rename {
        from: String,
        to: String,
    },
    Move {
        from: String,
        to: String,
    },
    Copy {
        from: String,
        to: String,
    },
    Mkdir {
        path: String,
    },
    Chmod {
        path: String,
        mode: String,
    },
    Append {
        path: String,
        content: String,
    },
    AppendOnce {
        path: String,
        content: String,
    },
    Prepend {
        path: String,
        content: String,
    },
    InsertBefore {
        path: String,
        marker: String,
        content: String,
    },
    InsertAfter {
        path: String,
        marker: String,
        content: String,
    },
    Replace {
        path: String,
        #[serde(with = "serde_regex")]
        replace: Regex,
        content: String,
        #[serde(default)]
        replace_all: bool,
        #[serde(default)]
        expected_matches: Option<usize>,
    },
    ReplaceOrAppend {
        path: String,
        #[serde(with = "serde_regex")]
        replace: Regex,
        content: String,
        #[serde(default)]
        replace_all: bool,
        #[serde(default)]
        expected_matches: Option<usize>,
    },
    ManagedBlock {
        path: String,
        start_marker: String,
        end_marker: String,
        content: String,
    },
}

#[derive(Clone, Copy, Deserialize, Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IfExists {
    Overwrite,
    Error,
    Skip,
}

pub type Array = Vec<Value>;
pub type Map = Vec<(String, Value)>;

#[derive(PartialEq, Clone, Debug)]
pub enum Value {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Array(Array),
    Map(Map),
}

impl Serialize for Value {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Value::String(v) => serializer.serialize_str(v),
            Value::Integer(v) => serializer.serialize_i64(*v),
            Value::Float(v) => serializer.serialize_f64(*v),
            Value::Boolean(v) => serializer.serialize_bool(*v),
            Value::Array(vec) => {
                let mut seq = serializer.serialize_seq(Some(vec.len()))?;
                for e in vec {
                    seq.serialize_element(e)?;
                }
                seq.end()
            }
            Value::Map(vec) => {
                let mut map = serializer.serialize_map(Some(vec.len()))?;
                for (k, v) in vec {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}

impl TryFrom<JsonValue> for Value {
    type Error = String;

    fn try_from(value: JsonValue) -> Result<Self, Self::Error> {
        match value {
            JsonValue::String(v) => Ok(Value::String(v)),
            JsonValue::Number(v) => json_number_to_value(v),
            JsonValue::Bool(v) => Ok(Value::Boolean(v)),
            JsonValue::Array(values) => {
                let mut result = Vec::with_capacity(values.len());
                for value in values {
                    result.push(Value::try_from(value)?);
                }
                Ok(Value::Array(result))
            }
            JsonValue::Object(map) => map_from_json(map).map(Value::Map),
            JsonValue::Null => Err("null is not supported".to_string()),
        }
    }
}

fn json_number_to_value(number: JsonNumber) -> Result<Value, String> {
    if let Some(int) = number.as_i64() {
        return Ok(Value::Integer(int));
    }
    number
        .as_f64()
        .map(Value::Float)
        .ok_or_else(|| "number is out of range".to_string())
}

fn map_from_json(map: JsonMap<String, JsonValue>) -> Result<Map, String> {
    map.into_iter()
        .map(|(key, value)| Value::try_from(value).map(|value| (key, value)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn converts_json_array() {
        let value = Value::try_from(json!(["a", 1, true])).expect("array should be converted");
        assert_eq!(
            value,
            Value::Array(vec![
                Value::String("a".to_string()),
                Value::Integer(1),
                Value::Boolean(true)
            ])
        );
    }

    #[test]
    fn converts_json_map() {
        let value = Value::try_from(json!({
            "items": [1, 2],
            "nested": { "flag": false }
        }))
        .expect("map should be converted");
        assert_eq!(
            value,
            Value::Map(vec![
                (
                    "items".to_string(),
                    Value::Array(vec![Value::Integer(1), Value::Integer(2)])
                ),
                (
                    "nested".to_string(),
                    Value::Map(vec![("flag".to_string(), Value::Boolean(false))])
                )
            ])
        );
    }

    #[test]
    fn rejects_null_json_value() {
        let error = Value::try_from(JsonValue::Null).expect_err("null is unsupported");
        assert!(error.contains("null"));
    }
}
