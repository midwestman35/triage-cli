# Common dev commands for triage-cli. Mirrors CLAUDE.md "Common commands".

set working-directory := "triage-cli-rs"

default:
    @just --list

test:
    cargo test --all-targets

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --all-targets -- -D warnings

check:
    just fmt-check && just lint && just test

build:
    cargo build --release

[working-directory: '.']
doctor:
    cargo run --release --manifest-path triage-cli-rs/Cargo.toml -- doctor

[working-directory: '.']
build-map:
    cargo run --release --manifest-path triage-cli-rs/Cargo.toml -- build-map
