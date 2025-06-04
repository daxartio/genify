# genify

[![Crates.io](https://img.shields.io/crates/v/genify.svg)](https://crates.io/crates/genify)
[![Docs.rs](https://docs.rs/genify/badge.svg)](https://docs.rs/genify)
[![CI](https://img.shields.io/github/actions/workflow/status/daxartio/genify/ci.yml?branch=main)](https://github.com/daxartio/genify/actions)
[![Coverage Status](https://coveralls.io/repos/github/daxartio/genify/badge.svg?branch=main)](https://coveralls.io/github/daxartio/genify?branch=main)

**Turn one file into a complete project**

The main idea is to have a single source file that can be used to generate or update a full project structure quickly and consistently using different configuration files.

There are two modes:

- **CLI interactive mode** – for manual use and quick input.
- **Code mode** – for automated and configurable project generation.

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

Usage: genify [OPTIONS] <PATH>

Arguments:
  <PATH>  Path to a config file or http(s) URL

Options:
  -n, --no-interaction  Do not ask any interactive question
  -h, --help            Print help
  -V, --version         Print version
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
type = "file"
path = "{{ dir }}/some.txt"  # if the file exists will be error
content = "{{ val }} {{ value }} {{ other | pascal_case }} {{ override }} - should be replaced"

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

`tmp/some.txt`

```
prepend value
val value Val 1 - replaced value
append value
```

### Supported Variable Types

| Type     | Description              | CLI Interactive Support  |
|----------|--------------------------|--------------------------|
| String   | Text value               | ✅ Supported             |
| Integer  | Whole number             | ✅ Supported             |
| Float    | Decimal number           | ✅ Supported             |
| Boolean  | true or false            | ✅ Supported             |
| Array    | List of values           | ❌ Not supported         |
| Map      | Key-value pairs          | ❌ Not supported         |

**Note:** In interactive CLI mode, all types are supported **except** `Array` and `Map`, will be used default values.


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
        .expect("Cannot parse the example.toml"),
        Some(vec![(
            "value".to_string(),
            genify::Value::String("val".to_string()),
        )]),
    )
    .expect("Cannot generate the example");
}
```

## License

* [MIT LICENSE](LICENSE)

## Contribution

[CONTRIBUTING.md](CONTRIBUTING.md)
