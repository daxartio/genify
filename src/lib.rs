mod error;
pub mod generation;
#[cfg(feature = "mcp")]
pub mod mcp;
mod schema;
mod tera_filters;
mod toml;

use regex::Regex;
use std::{
    borrow::Cow,
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
            Rule::Write {
                path,
                content,
                if_exists: _,
            } => {
                render_string(&mut tera, &context, path)?;
                render_string(&mut tera, &context, content)?;
            }
            Rule::Delete { path } | Rule::Mkdir { path } | Rule::Chmod { path, mode: _ } => {
                render_string(&mut tera, &context, path)?;
            }
            Rule::Rename { from, to } | Rule::Move { from, to } | Rule::Copy { from, to } => {
                render_string(&mut tera, &context, from)?;
                render_string(&mut tera, &context, to)?;
            }
            Rule::Append { path, content }
            | Rule::AppendOnce { path, content }
            | Rule::Prepend { path, content } => {
                render_string(&mut tera, &context, path)?;
                render_string(&mut tera, &context, content)?;
            }
            Rule::InsertBefore {
                path,
                marker,
                content,
            }
            | Rule::InsertAfter {
                path,
                marker,
                content,
            } => {
                render_string(&mut tera, &context, path)?;
                render_string(&mut tera, &context, marker)?;
                render_string(&mut tera, &context, content)?;
            }
            Rule::Replace {
                path,
                replace: _,
                content,
                replace_all: _,
                expected_matches: _,
            }
            | Rule::ReplaceOrAppend {
                path,
                replace: _,
                content,
                replace_all: _,
                expected_matches: _,
            } => {
                render_string(&mut tera, &context, path)?;
                render_string(&mut tera, &context, content)?;
            }
            Rule::ManagedBlock {
                path,
                start_marker,
                end_marker,
                content,
            } => {
                render_string(&mut tera, &context, path)?;
                render_string(&mut tera, &context, start_marker)?;
                render_string(&mut tera, &context, end_marker)?;
                render_string(&mut tera, &context, content)?;
            }
        }
    }

    Ok(config)
}

fn render_string(tera: &mut Tera, context: &Context, value: &mut String) -> Result<(), Error> {
    *value = tera.render_str(value, context).map_err(Error::Tera)?;
    Ok(())
}

pub fn extend_paths(mut config: Config, root: &Path) -> Result<Config, Error> {
    for rule in config.rules.iter_mut() {
        match rule {
            Rule::Write {
                path,
                content: _,
                if_exists: _,
            }
            | Rule::Delete { path }
            | Rule::Mkdir { path }
            | Rule::Chmod { path, mode: _ }
            | Rule::Append { path, content: _ }
            | Rule::AppendOnce { path, content: _ }
            | Rule::Prepend { path, content: _ }
            | Rule::InsertBefore {
                path,
                marker: _,
                content: _,
            }
            | Rule::InsertAfter {
                path,
                marker: _,
                content: _,
            }
            | Rule::ManagedBlock {
                path,
                start_marker: _,
                end_marker: _,
                content: _,
            } => extend_path(root, path),
            Rule::Replace {
                path,
                replace: _,
                content: _,
                replace_all: _,
                expected_matches: _,
            }
            | Rule::ReplaceOrAppend {
                path,
                replace: _,
                content: _,
                replace_all: _,
                expected_matches: _,
            } => extend_path(root, path),
            Rule::Rename { from, to } | Rule::Move { from, to } | Rule::Copy { from, to } => {
                extend_path(root, from);
                extend_path(root, to);
            }
        }
    }

    Ok(config)
}

fn extend_path(root: &Path, path: &mut String) {
    *path = root.join(path.as_str()).to_string_lossy().to_string();
}

pub fn generate_files(config: Config) -> Result<(), Error> {
    for rule in config.rules.iter() {
        match rule {
            Rule::Write {
                path,
                content,
                if_exists,
            } => {
                let path = Path::new(path);
                create_dir_all(path)?;
                match if_exists {
                    IfExists::Error => {
                        let mut file = fs::File::options()
                            .create_new(true)
                            .write(true)
                            .open(path)
                            .map_err(Error::IOError)?;
                        writeln!(&mut file, "{}", content.trim_end()).map_err(Error::IOError)?;
                    }
                    IfExists::Overwrite => {
                        fs::write(path, format!("{}\n", content.trim_end()))
                            .map_err(Error::IOError)?;
                    }
                    IfExists::Skip => {
                        if !path.exists() {
                            let mut file = fs::File::options()
                                .create_new(true)
                                .write(true)
                                .open(path)
                                .map_err(Error::IOError)?;
                            writeln!(&mut file, "{}", content.trim_end())
                                .map_err(Error::IOError)?;
                        }
                    }
                }
            }
            Rule::Delete { path } => {
                let path = Path::new(path);
                if path.is_dir() {
                    fs::remove_dir_all(path).map_err(Error::IOError)?;
                } else if path.exists() {
                    fs::remove_file(path).map_err(Error::IOError)?;
                }
            }
            Rule::Rename { from, to } | Rule::Move { from, to } => {
                let to = Path::new(to);
                create_dir_all(to)?;
                fs::rename(from, to).map_err(Error::IOError)?;
            }
            Rule::Copy { from, to } => {
                let to = Path::new(to);
                create_dir_all(to)?;
                fs::copy(from, to).map_err(Error::IOError)?;
            }
            Rule::Mkdir { path } => {
                fs::create_dir_all(path).map_err(Error::IOError)?;
            }
            Rule::Chmod { path, mode } => {
                set_mode(Path::new(path), parse_mode(mode)?)?;
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
            Rule::AppendOnce { path, content } => {
                let path = Path::new(path);
                create_dir_all(path)?;
                let mut file_content = read_file_or_empty(path)?;
                let block = content.trim_end();
                if !file_content.contains(block) {
                    file_content.push_str(&format!("{}\n", block));
                    fs::write(path, file_content).map_err(Error::IOError)?;
                }
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
            Rule::InsertBefore {
                path,
                marker,
                content,
            } => {
                let path = Path::new(path);
                create_dir_all(path)?;
                let file_content = read_file_or_empty(path)?;
                let updated = insert_before(&file_content, marker, content)
                    .ok_or_else(|| Error::Operation(format!("marker not found: {marker}")))?;
                fs::write(path, updated).map_err(Error::IOError)?;
            }
            Rule::InsertAfter {
                path,
                marker,
                content,
            } => {
                let path = Path::new(path);
                create_dir_all(path)?;
                let file_content = read_file_or_empty(path)?;
                let updated = insert_after(&file_content, marker, content)
                    .ok_or_else(|| Error::Operation(format!("marker not found: {marker}")))?;
                fs::write(path, updated).map_err(Error::IOError)?;
            }
            Rule::Replace {
                path,
                replace,
                content,
                replace_all,
                expected_matches,
            } => {
                let path = Path::new(path);
                create_dir_all(path)?;
                let file_content = read_file_or_empty(path)?;
                let replaced = replace_content(
                    &file_content,
                    replace,
                    content,
                    *replace_all,
                    *expected_matches,
                )?;
                fs::write(path, format!("{}\n", replaced.trim_end())).map_err(Error::IOError)?;
            }
            Rule::ReplaceOrAppend {
                path,
                replace,
                content,
                replace_all,
                expected_matches,
            } => {
                let path = Path::new(path);
                create_dir_all(path)?;
                let file_content = read_file_or_empty(path)?;
                let updated = if replace.is_match(&file_content) {
                    replace_content(
                        &file_content,
                        replace,
                        content,
                        *replace_all,
                        *expected_matches,
                    )?
                    .into_owned()
                } else {
                    format!("{}{}\n", file_content, content.trim_end())
                };
                fs::write(path, format!("{}\n", updated.trim_end())).map_err(Error::IOError)?;
            }
            Rule::ManagedBlock {
                path,
                start_marker,
                end_marker,
                content,
            } => {
                let path = Path::new(path);
                create_dir_all(path)?;
                let file_content = read_file_or_empty(path)?;
                let updated =
                    upsert_managed_block(&file_content, start_marker, end_marker, content)?;
                fs::write(path, format!("{}\n", updated.trim_end())).map_err(Error::IOError)?;
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

fn read_file_or_empty(path: &Path) -> Result<String, Error> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(Error::IOError(err)),
    }
}

fn replace_content<'a>(
    file_content: &'a str,
    replace: &Regex,
    content: &str,
    replace_all: bool,
    expected_matches: Option<usize>,
) -> Result<Cow<'a, str>, Error> {
    let matches = replace.find_iter(file_content).count();
    let expected = expected_matches.unwrap_or(1);
    if !replace_all && matches != expected {
        return Err(Error::Operation(format!(
            "expected {expected} matches, found {matches}"
        )));
    }

    if replace_all {
        Ok(replace.replace_all(file_content, content))
    } else {
        Ok(replace.replacen(file_content, 1, content))
    }
}

fn insert_before(file_content: &str, marker: &str, content: &str) -> Option<String> {
    let index = file_content.find(marker)?;
    let mut updated = String::with_capacity(file_content.len() + content.len() + 1);
    updated.push_str(&file_content[..index]);
    updated.push_str(content.trim_end());
    updated.push('\n');
    updated.push_str(&file_content[index..]);
    Some(updated)
}

fn insert_after(file_content: &str, marker: &str, content: &str) -> Option<String> {
    let index = file_content.find(marker)? + marker.len();
    let mut updated = String::with_capacity(file_content.len() + content.len() + 1);
    updated.push_str(&file_content[..index]);
    updated.push('\n');
    updated.push_str(content.trim_end());
    updated.push_str(&file_content[index..]);
    Some(updated)
}

fn upsert_managed_block(
    file_content: &str,
    start_marker: &str,
    end_marker: &str,
    content: &str,
) -> Result<String, Error> {
    let block = format!("{start_marker}\n{}\n{end_marker}", content.trim_end());
    let start = file_content.find(start_marker);
    let end = file_content.find(end_marker);

    match (start, end) {
        (Some(start), Some(end)) if start <= end => {
            let end_index = end + end_marker.len();
            Ok(format!(
                "{}{}{}",
                &file_content[..start],
                block,
                &file_content[end_index..]
            ))
        }
        (None, None) => Ok(format!("{}{}\n", file_content, block)),
        _ => Err(Error::Operation(
            "managed block requires both start_marker and end_marker".to_string(),
        )),
    }
}

fn parse_mode(mode: &str) -> Result<u32, Error> {
    u32::from_str_radix(mode.trim_start_matches("0o"), 8)
        .map_err(|err| Error::Operation(format!("invalid chmod mode `{mode}`: {err}")))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;

    let permissions = fs::Permissions::from_mode(mode);
    fs::set_permissions(path, permissions).map_err(Error::IOError)
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), Error> {
    Err(Error::Operation(
        "chmod is only supported on Unix platforms".to_string(),
    ))
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
                type = "write"
                path = "cmd/main.go"
                content = ""
                if_exists = "error"
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
                type = "write"
                path = "{{ dir }}/some.txt"
                content = "{{ val }} {{ value | pascal_case }} {{ other }} {{ override }} - should be replaced"
                if_exists = "error"

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
