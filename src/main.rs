use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use clap::{Args, CommandFactory, Parser, Subcommand, error::ErrorKind};
use genify::generate_files;
use reqwest::blocking::Client;
use serde_json::Value as JsonValue;
use url::Url;

static BIN_NAME: &str = env!("CARGO_PKG_NAME");
static BIN_DESCRIPTION: &str = env!("CARGO_PKG_DESCRIPTION");

#[derive(Parser)]
#[command(version, name=BIN_NAME, about=BIN_DESCRIPTION, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    /// Path to a config file or http(s) URL.
    path: Option<ConfigPath>,
    /// Do not ask any interactive question.
    #[arg(short, long)]
    no_interaction: bool,
    /// Override props using a JSON object (Array/Map supported).
    #[arg(short = 'p', long = "props-json", value_name = "JSON")]
    props_json: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start genify as an MCP server over STDIO.
    Mcp(McpArgs),
}

#[derive(Args)]
struct McpArgs {
    /// Filesystem root the MCP server is allowed to access.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Disable tools that write to disk.
    #[arg(long)]
    read_only: bool,
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
    let cmd = Cli::command();
    let Cli {
        command,
        path,
        no_interaction,
        props_json,
    } = cli;

    if let Some(command) = command {
        match command {
            Commands::Mcp(args) => {
                if let Err(error) = genify::mcp::serve_stdio(&args.root, args.read_only) {
                    eprintln!("Failed to run MCP server: {error}");
                    std::process::exit(1);
                }
            }
        }
        return;
    }

    let Some(path) = path else {
        clap::Error::raw(
            ErrorKind::MissingRequiredArgument,
            "the following required argument was not provided: <PATH>",
        )
        .with_cmd(&cmd)
        .exit();
    };

    let mut config = parse_file(&path).unwrap_or_else(|err| err.with_cmd(&cmd).exit());

    if let Some(raw_props) = &props_json {
        let overrides = parse_props_json(raw_props).unwrap_or_else(|err| err.with_cmd(&cmd).exit());
        config.props.extend(overrides);
    }

    if let Err(error) = genify::render_config_props_with_func(config, |k, v| {
        if no_interaction {
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
            genify::Value::Array(_) | genify::Value::Map(_) => {
                let Some(default) = value_to_json_string(v) else {
                    return;
                };
                if let Some(new) = prompt(&default) {
                    match parse_json_value(&new) {
                        Ok(genify::Value::Array(parsed)) => *v = genify::Value::Array(parsed),
                        Ok(genify::Value::Map(parsed)) => *v = genify::Value::Map(parsed),
                        Ok(_) => eprintln!(
                            "Value for \"{k}\" must be a JSON array or object; keeping default."
                        ),
                        Err(err) => eprintln!("Failed to parse \"{k}\": {err}"),
                    }
                }
            }
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
        .with_cmd(&cmd)
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

fn parse_props_json(raw: &str) -> Result<genify::Map, clap::Error> {
    let parsed: JsonValue = serde_json::from_str(raw).map_err(|err| {
        clap::Error::raw(
            ErrorKind::InvalidValue,
            format!("Failed to parse props JSON: {err}"),
        )
    })?;
    let object = parsed.as_object().ok_or_else(|| {
        clap::Error::raw(ErrorKind::InvalidValue, "Props JSON must be a JSON object")
    })?;

    let mut props = Vec::with_capacity(object.len());
    for (key, value) in object {
        let converted = genify::Value::try_from(value.clone()).map_err(|err| {
            clap::Error::raw(
                ErrorKind::InvalidValue,
                format!("Invalid value for \"{key}\": {err}"),
            )
        })?;
        props.push((key.clone(), converted));
    }

    Ok(props)
}

fn parse_json_value(raw: &str) -> Result<genify::Value, String> {
    let value: JsonValue =
        serde_json::from_str(raw).map_err(|err| format!("invalid JSON: {err}"))?;
    genify::Value::try_from(value)
}

fn value_to_json_string(value: &genify::Value) -> Option<String> {
    serde_json::to_string(value).ok()
}
