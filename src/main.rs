use std::{
    fs,
    io::{self, Write},
    path::Path,
};

use clap::{error::ErrorKind, CommandFactory, Parser};
use genify::generate_files;
use reqwest::blocking::Client;
use url::Url;

static BIN_NAME: &str = env!("CARGO_PKG_NAME");
static BIN_DESCRIPTION: &str = env!("CARGO_PKG_DESCRIPTION");

#[derive(Parser)]
#[command(version, name=BIN_NAME, about=BIN_DESCRIPTION)]
struct Cli {
    /// Path to a config file or http(s) URL.
    path: ConfigPath,
    /// Do not ask any interactive question.
    #[arg(short, long)]
    no_interaction: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ConfigPath {
    File(String),
    Http(Url),
}

impl From<&str> for ConfigPath {
    fn from(value: &str) -> Self {
        if let Ok(url) = Url::parse(value) {
            if url.scheme() == "http" || url.scheme() == "https" {
                return Self::Http(url);
            }
        }
        Self::File(value.to_string())
    }
}

fn main() {
    let cli = Cli::parse();
    let cmd = &Cli::command();

    let config = parse_file(&cli.path).unwrap_or_else(|err| err.with_cmd(cmd).exit());

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

fn parse_file(path: &ConfigPath) -> Result<genify::Config, clap::Error> {
    let raw = match path {
        ConfigPath::File(p) => {
            let path = Path::new(p);
            if !path.is_file() {
                return Err(clap::Error::raw(
                    ErrorKind::ValueValidation,
                    "Path is not a file",
                ));
            }
            fs::read_to_string(path)
                .map_err(|_| clap::Error::raw(ErrorKind::ValueValidation, "Failed to read file"))?
        }
        ConfigPath::Http(url) => {
            let client = Client::new();
            client
                .get(url.clone())
                .send()
                .and_then(|r| r.error_for_status()) // handle HTTP errors
                .and_then(|r| r.text())
                .map_err(|e| {
                    clap::Error::raw(
                        ErrorKind::ValueValidation,
                        format!("Failed to fetch URL: {e}"),
                    )
                })?
        }
    };

    let config = genify::parse_toml(&raw).map_err(|err| {
        clap::Error::raw(
            ErrorKind::ValueValidation,
            format!("Failed to parse TOML: {err:?}"),
        )
    })?;
    Ok(config)
}
