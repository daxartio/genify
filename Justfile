all: fmt check test

target_dir := env_var_or_default("CARGO_TARGET_DIR", "target")

fmt:
    cargo +nightly fmt --all --

format: fmt

check:
    cargo +nightly fmt --all -- --check
    cargo clippy --all-targets --all-features --workspace

lint: check

test:
    cargo test --all-features --all-targets --workspace

build crate:
    cargo build -p {{crate}} --release

build-dev crate:
    cargo build -p {{crate}}

run crate *args:
    ./{{target_dir}}/release/{{crate}} {{args}}

run-dev crate *args:
    cargo run -p {{crate}} -- {{args}}
