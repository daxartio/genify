use std::{
    fs,
    io::{self, Write},
    path::Path,
};

use clap::{error::ErrorKind, CommandFactory, Parser};
use genify::generate_files;

static BIN_NAME: &str = env!("CARGO_PKG_NAME");
static BIN_DESCRIPTION: &str = env!("CARGO_PKG_DESCRIPTION");

#[derive(Parser)]
#[command(version, name=BIN_NAME, about=BIN_DESCRIPTION)]
struct Cli {
    /// Path to a config file.
    path: ConfigPath,
    /// Do not ask any interactive question.
    #[arg(short, long)]
    no_interaction: bool,
}

fn main() {
    let cli = Cli::parse();
    let cmd = &Cli::command();

    let config = match &cli.path {
        ConfigPath::File(path) => parse_file(cmd, path).unwrap_or_else(|err| err.exit()),
    };
    if let Err(error) = genify::render_config_props_with_func(config, |k, v| {
        if cli.no_interaction {
            return;
        }
        let prompt = |default: &str| -> Option<String> {
            print!("{} ({}): ", k, default);
            if io::stdout().flush().is_err() {
                return None;
            }
            let mut input = String::new();
            if io::stdin().read_line(&mut input).is_ok() {
                let trimmed = input.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
            None
        };

        match v {
            genify::Value::String(s) => {
                if let Some(new) = prompt(s) {
                    *v = genify::Value::String(new);
                }
            }
            genify::Value::Integer(i) => {
                if let Some(new) = prompt(&i.to_string()) {
                    if let Ok(parsed) = new.parse::<i64>() {
                        *v = genify::Value::Integer(parsed);
                    }
                }
            }
            genify::Value::Float(f) => {
                if let Some(new) = prompt(&f.to_string()) {
                    if let Ok(parsed) = new.parse::<f64>() {
                        *v = genify::Value::Float(parsed);
                    }
                }
            }
            genify::Value::Boolean(b) => {
                if let Some(new) = prompt(&b.to_string()) {
                    if let Ok(parsed) = new.parse::<bool>() {
                        *v = genify::Value::Boolean(parsed);
                    }
                }
            }
            genify::Value::Array(_) | genify::Value::Map(_) => {}
        }
    })
    .and_then(genify::render_config_rules)
    .and_then(|c| genify::extend_paths(c, Path::new(".")))
    .and_then(generate_files)
    {
        clap::Error::raw(
            ErrorKind::InvalidValue,
            format!("Failed to process config: {error:?}"),
        )
        .with_cmd(cmd)
        .exit();
    };
}

fn parse_file(cmd: &clap::Command, path: &String) -> Result<genify::Config, clap::Error> {
    let path = Path::new(path);
    if !path.is_file() {
        return Err(
            clap::Error::raw(ErrorKind::ValueValidation, "Path is not a file").with_cmd(cmd),
        );
    }
    let raw = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => {
            return Err(
                clap::Error::raw(ErrorKind::ValueValidation, "Failed to read file").with_cmd(cmd),
            );
        }
    };

    let config = match genify::parse_toml(&raw) {
        Ok(cfg) => cfg,
        Err(err) => {
            return Err(clap::Error::raw(
                ErrorKind::ValueValidation,
                format!("Failed to parse TOML: {err:?}"),
            )
            .with_cmd(cmd));
        }
    };
    Ok(config)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ConfigPath {
    File(String),
}

impl From<&str> for ConfigPath {
    fn from(value: &str) -> Self {
        Self::File(value.to_string())
    }
}
