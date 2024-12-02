use std::{
    fs,
    io::{self, Write},
    path::Path,
};

use clap::{
    error::{ContextKind, ContextValue, ErrorKind},
    CommandFactory, Parser,
};
use genify::generate_files;

static BIN_NAME: &str = env!("CARGO_PKG_NAME");
static BIN_DESCRIPTION: &str = env!("CARGO_PKG_DESCRIPTION");

#[derive(Parser)]
#[command(version, name=BIN_NAME, about=BIN_DESCRIPTION)]
struct Cli {
    path: ConfigPath,
    /// Do not ask any interactive question.
    #[arg(short, long)]
    no_interaction: bool,
}

fn main() {
    let cli = Cli::parse();
    let mut err = clap::Error::new(ErrorKind::ValueValidation).with_cmd(&Cli::command());

    let mut config = match &cli.path {
        ConfigPath::File(path) => {
            let path = Path::new(path);
            if !path.is_file() {
                err.insert(ContextKind::InvalidValue, ContextValue::None);
                err.exit();
            }
            let Ok(Ok(config)) =
                fs::read_to_string(path).map(|raw| genify::Config::from_toml(&raw))
            else {
                err.insert(ContextKind::InvalidValue, ContextValue::None);
                err.exit();
            };
            config
        }
    };
    let Ok(_) = genify::render_config_props_with_func(&mut config, |k, v| {
        if cli.no_interaction {
            return;
        }
        if let toml::Value::String(default) = v {
            print!("{} ({}): ", k, default);
            io::stdout().flush().unwrap();
            let mut input_string = String::new();
            io::stdin().read_line(&mut input_string).unwrap();
            if !input_string.trim().is_empty() {
                *v = toml::Value::String(input_string.trim().to_string());
            }
        }
    })
    .and_then(genify::render_config_rules)
    .and_then(|c| genify::extend_paths(c, Path::new(".")))
    .and_then(generate_files) else {
        err.insert(ContextKind::InvalidValue, ContextValue::None);
        err.exit();
    };
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
