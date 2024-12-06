use regex::Regex;
use serde::{
    ser::{SerializeMap, SerializeSeq},
    Deserialize, Serialize, Serializer,
};

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
    File {
        path: String,
        content: String,
    },
    Append {
        path: String,
        content: String,
    },
    Prepend {
        path: String,
        content: String,
    },
    Replace {
        path: String,
        #[serde(with = "serde_regex")]
        replace: Regex,
        content: String,
    },
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
