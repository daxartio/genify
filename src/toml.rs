use serde::{Deserialize, Serialize};

pub fn parse_toml(raw: &str) -> Result<crate::Config, toml::de::Error> {
    Ok(Config::parse(raw)?.into())
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub(crate) struct Config {
    #[serde(default)]
    pub props: toml::map::Map<String, toml::Value>,
    #[serde(default)]
    pub rules: Vec<crate::schema::Rule>,
}

impl Config {
    pub(crate) fn parse(raw: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(raw)
    }
}

impl From<Config> for crate::Config {
    fn from(value: Config) -> Self {
        Self {
            props: value
                .props
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect(),
            rules: value.rules,
        }
    }
}

impl From<toml::Value> for crate::Value {
    fn from(value: toml::Value) -> Self {
        match value {
            toml::Value::String(v) => crate::Value::String(v),
            toml::Value::Integer(v) => crate::Value::Integer(v),
            toml::Value::Float(v) => crate::Value::Float(v),
            toml::Value::Boolean(v) => crate::Value::Boolean(v),
            toml::Value::Datetime(datetime) => crate::Value::String(datetime.to_string()),
            toml::Value::Array(vec) => {
                crate::Value::Array(vec.into_iter().map(|v| v.into()).collect())
            }
            toml::Value::Table(map) => {
                crate::Value::Map(map.into_iter().map(|(k, v)| (k, v.into())).collect())
            }
        }
    }
}
