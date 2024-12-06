mod error;
mod schema;
mod tera_filters;
mod toml;

use std::{
    fs,
    io::{self, Read, Seek, Write},
    path::Path,
};
use tera::{Context, Tera};

pub use crate::error::*;
pub use crate::schema::*;
pub use crate::toml::parse_toml;

pub fn generate(root: &Path, config: &Config, overrides: Option<Map>) -> Result<(), Error> {
    let config = config.clone();

    render_config_props(config)
        .and_then(|c| apply_overrides(c, overrides))
        .and_then(render_config_rules)
        .and_then(|c| extend_paths(c, root))
        .and_then(generate_files)
}

fn apply_overrides(mut config: Config, overrides: Option<Map>) -> Result<Config, Error> {
    if let Some(overrides) = overrides {
        config.props.extend(overrides);
    }
    Ok(config)
}

pub fn render_config_props(config: Config) -> Result<Config, Error> {
    render_config_props_with_func(config, |_, _| {})
}

pub fn render_config_props_with_func(
    mut config: Config,
    mut func: impl FnMut(&String, &mut Value),
) -> Result<Config, Error> {
    let mut tera = Tera::default();
    tera_filters::register_all(&mut tera);

    let mut context = Context::new();

    for (key, val) in config.props.iter_mut() {
        if let Value::String(s) = val {
            *s = tera.render_str(s, &context).map_err(Error::Tera)?;
        }
        func(key, val);
        context.insert(key.as_str(), val);
    }

    Ok(config)
}

pub fn render_config_rules(mut config: Config) -> Result<Config, Error> {
    let mut tera = Tera::default();
    tera_filters::register_all(&mut tera);

    let mut context = Context::new();

    for (key, val) in config.props.iter_mut() {
        context.insert(key.as_str(), val);
    }

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

pub fn extend_paths(mut config: Config, root: &Path) -> Result<Config, Error> {
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

pub fn generate_files(config: Config) -> Result<(), Error> {
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
        let config = parse_toml("");
        assert!(config.is_ok());
    }

    #[test]
    fn test_parse() {
        let config = parse_toml(
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

        let config: Config = parse_toml(
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

        generate(
            Path::new("."),
            &config,
            Some(vec![("override".to_string(), Value::Integer(2))]),
        )
        .expect("File should be generated");

        let result = fs::read_to_string("tmp/some.txt").expect("File should be read");
        assert_eq!(
            "prepend value\nval Value val 2 - replaced value\nappend value\n",
            result
        );
    }
}
