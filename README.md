# genify

**MCP tool for AI coding agents to safely plan, diff, and apply structured file changes inside a project root.**

Use `genify` for repeatable codebase edits: writing files, replacing text, inserting blocks around markers, moving/copying/deleting paths, creating directories, changing modes, and generating dry-run diffs before applying changes.

[![Crates.io](https://img.shields.io/crates/v/genify.svg)](https://crates.io/crates/genify)
[![Docs.rs](https://docs.rs/genify/badge.svg)](https://docs.rs/genify)
[![CI](https://img.shields.io/github/actions/workflow/status/daxartio/genify/ci.yml?branch=main)](https://github.com/daxartio/genify/actions)
[![Coverage Status](https://coveralls.io/repos/github/daxartio/genify/badge.svg?branch=main)](https://coveralls.io/github/daxartio/genify?branch=main)

**Turn one file into a complete project**

The main idea is to have a single source file that can be used to generate or update a full project structure quickly and consistently using different configuration files.

There are three modes:

- **CLI interactive mode** – for manual use and quick input.
- **Code mode** – for automated and configurable project generation.
- **MCP server mode** – for AI coding agents that call structured tools over STDIO.

Features:

- Create files from templates based on config.
- Replace content using regular expressions.
- Append or prepend content to existing files.
- Easily update multiple projects using shared configs.

## Installation

### Cargo

* Install the rust toolchain in order to have cargo installed by following
  [this](https://www.rust-lang.org/tools/install) guide.
* run
  ```
  cargo install genify --features cli
  ```

## Get Started

### CLI

```
Turn one file into a complete project

Usage: genify [OPTIONS] [PATH]
       genify <COMMAND>

Arguments:
  [PATH]  Path to a config file or http(s) URL

Options:
  -p, --props-json <JSON>  Override props using a JSON object (Array/Map supported)
  -n, --no-interaction     Do not ask any interactive question
  -h, --help               Print help
  -V, --version            Print version

Commands:
  mcp   Start genify as an MCP server over STDIO
  help  Print this message or the help of the given subcommand(s)
```

`example.toml`

```toml
[props]
value = "value"
dir = "tmp"
val = "val"
other = "{{ val }}"
override = "1"

[[rules]]
type = "write"
path = "{{ dir }}/some.txt"  # if the file exists will be error
content = "{{ val }} {{ value }} {{ other | pascal_case }} {{ override }} - should be replaced"
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
```

```shell
genify example.toml
```

Override props from the CLI (including Array/Map) using JSON:

```shell
genify example.toml --props-json '{"tags": ["cli", "json"], "meta": {"license": "MIT"}}'
```

`tmp/some.txt`

```
prepend value
val value Val 1 - replaced value
append value
```

### MCP

Run genify as an MCP server using the standard STDIO transport:

```shell
genify mcp --root .
```

For read-only agent sessions:

```shell
genify mcp --root . --read-only
```

Example Codex configuration:

```toml
[mcp_servers.genify]
command = "genify"
args = ["mcp", "--root", "."]
```

The MCP server writes protocol messages only to `stdout`; logs and diagnostics go to `stderr`.
All filesystem access is constrained to `--root`.

Available tools:

| Tool                     | Behavior                                                                                 |
|--------------------------|------------------------------------------------------------------------------------------|
| `genify_plan`            | Returns planned file operations and affected paths without writing files.                |
| `genify_diff`            | Runs generation in dry-run mode and returns a unified diff.                              |
| `genify_apply`           | Applies changes only when `explicit_approval` is `true` or `confirm_token` is `"apply"`. |
| `genify_validate_config` | Validates config parsing, rendering, and generated paths.                                |
| `genify_list_templates`  | Lists `.toml` configs and template files under the MCP root.                             |

`genify_plan`, `genify_diff`, and `genify_apply` accept the config directly as JSON in MCP tool arguments.
No temporary TOML config or template file is required.

```json
{
  "config": {
    "rules": [
      {
        "type": "replace",
        "path": "src/application.rs",
        "replace": "old text",
        "content": "new text"
      }
    ]
  },
  "root": "."
}
```

Supported rule types:

| Type                                 | Fields                                                                            |
|--------------------------------------|-----------------------------------------------------------------------------------|
| `write`                              | `path`, `content`, `if_exists` (`overwrite`, `error`, or `skip`)                  |
| `delete`                             | `path`                                                                            |
| `rename` / `move`                    | `from`, `to`                                                                      |
| `copy`                               | `from`, `to`                                                                      |
| `mkdir`                              | `path`                                                                            |
| `chmod`                              | `path`, `mode`                                                                    |
| `append` / `append_once` / `prepend` | `path`, `content`                                                                 |
| `insert_before` / `insert_after`     | `path`, `marker`, `content`                                                       |
| `replace` / `replace_or_append`      | `path`, `replace`, `content`, optional `replace_all`, optional `expected_matches` |
| `managed_block`                      | `path`, `start_marker`, `end_marker`, `content`                                   |

`replace` is strict by default: when `replace_all` is false or omitted, it fails unless the regex match count equals `expected_matches`, which defaults to `1`.

Supported rule examples:

```json
{
  "config": {
    "rules": [
      {
        "type": "write",
        "path": "new/file.rs",
        "content": "...",
        "if_exists": "error"
      }
    ]
  }
}
```

```json
{
  "config": {
    "rules": [
      {
        "type": "append_once",
        "path": "README.md",
        "content": "..."
      }
    ]
  }
}
```

```json
{
  "config": {
    "rules": [
      {
        "type": "managed_block",
        "path": "README.md",
        "start_marker": "<!-- genify:start -->",
        "end_marker": "<!-- genify:end -->",
        "content": "..."
      }
    ]
  }
}
```

```json
{
  "config": {
    "rules": [
      {
        "type": "insert_after",
        "path": "README.md",
        "marker": "## Usage",
        "content": "..."
      }
    ]
  }
}
```

```json
{
  "config": {
    "rules": [
      {
        "type": "move",
        "from": "old/file.rs",
        "to": "new/file.rs"
      }
    ]
  }
}
```

`genify_validate_config` returns a structured `hint.minimal_config` and per-operation examples when the config is missing or invalid.

### Supported Variable Types

| Type    | Description     | CLI Interactive Support |
|---------|-----------------|-------------------------|
| String  | Text value      | ✅ Supported             |
| Integer | Whole number    | ✅ Supported             |
| Float   | Decimal number  | ✅ Supported             |
| Boolean | true or false   | ✅ Supported             |
| Array   | List of values  | ✅ JSON input            |
| Map     | Key-value pairs | ✅ JSON input            |

**Note:** For `Array` and `Map`, enter JSON when prompted or provide them up front with `--props-json`.


### Code

```rust
fn main() {
    genify::generate(
        Path::new("."),
        &genify::parse_toml(
            fs::read_to_string("example.toml")
                .unwrap()
                .as_str(),
        )
        .expect("The config should be valid"),
        Some(vec![(
            "value".to_string(),
            genify::Value::String("val".to_string()),
        )]),
    )
    .expect("The generation should be successful");
}
```

## License

* [MIT LICENSE](LICENSE)

## Contribution

[CONTRIBUTING.md](CONTRIBUTING.md)
