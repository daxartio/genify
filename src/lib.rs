mod tera_filters;

use std::{
    fs,
    io::{self, Read, Seek, Write},
    path::Path,
};

use regex::Regex;
use serde::Deserialize;
use tera::{Context, Tera};

#[derive(Clone, Deserialize, Debug)]
pub struct Config {
    #[serde(default)]
    pub props: toml::map::Map<String, toml::Value>,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

impl Config {
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }
}

#[derive(Clone, Deserialize, Debug)]
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

#[derive(Debug)]
pub enum Error {
    Tera(tera::Error),
    IOError(io::Error),
}

pub fn generate(
    root: &Path,
    config: &Config,
    overrides: Option<toml::map::Map<String, toml::Value>>,
) -> Result<(), Error> {
    let mut config = config.clone();

    render_config_props(&mut config)
        .and_then(|c| apply_overrides(c, overrides))
        .and_then(render_config_rules)
        .and_then(|c| extend_paths(c, root))
        .and_then(generate_files)
}

fn apply_overrides(
    config: &mut Config,
    overrides: Option<toml::map::Map<String, toml::Value>>,
) -> Result<&mut Config, Error> {
    if let Some(overrides) = overrides {
        config.props.extend(overrides);
    }
    Ok(config)
}

pub fn render_config_props(config: &mut Config) -> Result<&mut Config, Error> {
    render_config_props_with_func(config, |_, _| {})
}

pub fn render_config_props_with_func(
    config: &mut Config,
    mut func: impl FnMut(&String, &mut toml::Value),
) -> Result<&mut Config, Error> {
    let mut tera = Tera::default();
    tera_filters::register_all(&mut tera);

    let mut context = Context::new();

    for (key, val) in config.props.iter_mut() {
        if let toml::Value::String(s) = val {
            *s = tera.render_str(s, &context).map_err(Error::Tera)?;
        }
        func(key, val);
        context.insert(key, val);
    }

    Ok(config)
}

pub fn render_config_rules(config: &mut Config) -> Result<&mut Config, Error> {
    let mut tera = Tera::default();
    tera_filters::register_all(&mut tera);

    let context = Context::from_serialize(&config.props).map_err(Error::Tera)?;

    for rule in config.rules.iter_mut() {
        match rule {
            Rule::File { path, content } => {
                *path = tera.render_str(path, &context).map_err(Error::Tera)?;

                *content = tera.render_str(content, &context).map_err(Error::Tera)?;
            }
            Rule::Append { path, content } => {
                *path = tera.render_str(path, &context).map_err(Error::Tera)?;

                *content = tera.render_str(content, &context).map_err(Error::Tera)?;
            }
            Rule::Prepend { path, content } => {
                *path = tera.render_str(path, &context).map_err(Error::Tera)?;

                *content = tera.render_str(content, &context).map_err(Error::Tera)?;
            }
            Rule::Replace {
                path,
                replace: _,
                content,
            } => {
                *path = tera.render_str(path, &context).map_err(Error::Tera)?;

                *content = tera.render_str(content, &context).map_err(Error::Tera)?;
            }
        }
    }

    Ok(config)
}

pub fn extend_paths<'a>(config: &'a mut Config, root: &Path) -> Result<&'a mut Config, Error> {
    for rule in config.rules.iter_mut() {
        match rule {
            Rule::File { path, content: _ } => {
                *path = root.join(path.as_str()).to_string_lossy().to_string();
            }
            Rule::Append { path, content: _ } => {
                *path = root.join(path.as_str()).to_string_lossy().to_string();
            }
            Rule::Prepend { path, content: _ } => {
                *path = root.join(path.as_str()).to_string_lossy().to_string();
            }
            Rule::Replace {
                path,
                replace: _,
                content: _,
            } => {
                *path = root.join(path.as_str()).to_string_lossy().to_string();
            }
        }
    }

    Ok(config)
}

pub fn generate_files(config: &mut Config) -> Result<(), Error> {
    for rule in config.rules.iter() {
        match rule {
            Rule::File { path, content } => {
                let path = Path::new(path);
                create_dir_all(path)?;
                let mut file = fs::File::options()
                    .create_new(true)
                    .write(true)
                    .open(path)
                    .map_err(Error::IOError)?;

                writeln!(&mut file, "{}", content.trim_end()).map_err(Error::IOError)?;
            }
            Rule::Append { path, content } => {
                let path = Path::new(path);
                create_dir_all(path)?;
                let mut file = fs::File::options()
                    .create(true)
                    .append(true)
                    .open(path)
                    .map_err(Error::IOError)?;

                writeln!(&mut file, "{}", content.trim_end()).map_err(Error::IOError)?;
            }
            Rule::Prepend { path, content } => {
                let path = Path::new(path);
                create_dir_all(path)?;
                let mut file = fs::File::options()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .open(path)
                    .map_err(Error::IOError)?;
                let mut file_content = String::new();
                file.read_to_string(&mut file_content)
                    .map_err(Error::IOError)?;
                file.seek(io::SeekFrom::Start(0)).map_err(Error::IOError)?;

                writeln!(&mut file, "{}\n{}", content, file_content.trim_end())
                    .map_err(Error::IOError)?;
            }
            Rule::Replace {
                path,
                replace,
                content,
            } => {
                let path = Path::new(path);
                create_dir_all(path)?;
                let file_content = {
                    let mut file = fs::File::options()
                        .create(true)
                        .read(true)
                        .write(true)
                        .truncate(false)
                        .open(path)
                        .map_err(Error::IOError)?;
                    let mut file_content = String::new();
                    file.read_to_string(&mut file_content)
                        .map_err(Error::IOError)?;
                    file_content
                };
                let mut file = fs::File::options()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(path)
                    .map_err(Error::IOError)?;

                let replaced = replace.replacen(&file_content, 1, content);
                writeln!(&mut file, "{}", replaced.trim_end()).map_err(Error::IOError)?;
            }
        }
    }
    Ok(())
}

fn create_dir_all(path: &Path) -> Result<(), Error> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };

    fs::create_dir_all(parent).map_err(Error::IOError)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty() {
        let config = Config::from_toml("");
        assert!(config.is_ok());
    }

    #[test]
    fn test_parse() {
        let config = Config::from_toml(
            r#"
                [props]
                project_name = "project"

                [[rules]]
                type = "file"
                path = "cmd/main.go"
                content = ""
            "#,
        );
        assert!(config.is_ok());
    }

    #[test]
    fn test_generate() {
        if let Err(err) = fs::remove_dir_all("tmp") {
            match err.kind() {
                io::ErrorKind::NotFound => {}
                _ => panic!("Tmp dir should be removed: {:?}", err),
            }
        }

        let config: Config = toml::from_str(
            r#"
                [props]
                value = "value"
                dir = "tmp"
                val = "val"
                other = "{{ val }}"
                override = "1"

                [[rules]]
                type = "file"
                path = "{{ dir }}/some.txt"
                content = "{{ val }} {{ value | pascal_case }} {{ other }} {{ override }} - should be replaced"

                [[rules]]
                type = "replace"
                path = "{{ dir }}/some.txt"
                replace = "should.*replaced"
                content = "replaced {{ value }}"

                [[rules]]
                type = "prepend"
                path = "{{ dir }}/some.txt"
                content = "prepend {{ value }}"

                [[rules]]
                type = "append"
                path = "{{ dir }}/some.txt"
                content = "append {{ value }}"
            "#,
        )
        .expect("Config should be parsed");

        let mut overrides = toml::map::Map::new();
        overrides.insert("override".to_string(), toml::Value::Integer(2));

        generate(Path::new("."), &config, Some(overrides)).expect("File should be generated");

        let result = fs::read_to_string("tmp/some.txt").expect("File should be read");
        assert_eq!(
            "prepend value\nval Value val 2 - replaced value\nappend value\n",
            result
        );
    }
}
