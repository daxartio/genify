# genify

[![Crates.io](https://img.shields.io/crates/v/genify.svg)](https://crates.io/crates/genify)
[![Docs.rs](https://docs.rs/genify/badge.svg)](https://docs.rs/genify)
[![CI](https://img.shields.io/github/actions/workflow/status/daxartio/genify/ci.yml?branch=main)](https://github.com/daxartio/genify/actions)
[![Coverage Status](https://coveralls.io/repos/github/daxartio/genify/badge.svg?branch=main)](https://coveralls.io/github/daxartio/genify?branch=main)

Turn one file into a complete project.

## Installation

### Cargo

* Install the rust toolchain in order to have cargo installed by following
  [this](https://www.rust-lang.org/tools/install) guide.
* run
  ```
  cargo install genify --features clap
  ```

## Get Started

### CLI

```
Turn one file into a complete project

Usage: genify [OPTIONS] <PATH>

Arguments:
  <PATH>

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

### Code

```rust
fn main() {
    genify::generate(
        Path::new("."),
        &genify::parse_toml(
            fs::read_to_string("xtask/templates/controller.toml")
                .unwrap()
                .as_str(),
        )
        .expect("Cannot parse the controller.toml"),
        Some(vec![(
            "name".to_string(),
            genify::Value::String(name.clone()),
        )]),
    )
    .expect("Cannot generate the controller");
}
```

## License

* [MIT LICENSE](LICENSE)

## Contribution

[CONTRIBUTING.md](CONTRIBUTING.md)
