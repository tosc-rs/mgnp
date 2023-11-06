#!/usr/bin/env -S just --justfile
_docstring := "
justfile for MnemOS
see https://just.systems/man for more details

Available variables:
    toolchain               # overrides the default Rust toolchain set in the
                            # rust-toolchain.toml file.
    no-nextest              # disables cargo nextest (use cargo test) instead.
    profile                 # configures what Cargo profile (release or debug)
                            # to use for builds.

Environment variables and defaults:
    RUST_LOG='debug'            # sets the log level for normal tests & tools.

    LOOM_LOG='debug'            # sets the log level for Loom tests.
    LOOM_LOCATION='true'        # enables location tracking in Loom tests.
    LOOM_MAX_PREEMPTIONS='2'    # sets maximum number of preemptions in Loom.

    # configures Miri behavior when running Miri tests
    MIRIFLAGS='-Zmiri-strict-provenance -Zmiri-disable-isolation'

Variables can be set using `just VARIABLE=VALUE ...` or
`just --set VARIABLE VALUE ...`.

See https://just.systems/man/en/chapter_36.html for details.
"

# Overrides the default Rust toolchain set in `rust-toolchain.toml`.
toolchain := ""

# disables cargo nextest
no-nextest := ''

# configures what profile to use for builds. the default depends on the target
# being built.
profile := 'release'

_cargo := "cargo" + if toolchain != "" { " +" + toolchain } else { "" }
_testcmd := if no-nextest != "" { "test" } else { "nextest run" }

_rust_log := "RUST_LOG=" + env_var_or_default("RUST_LOG", "debug")

# If we're running in Github Actions and cargo-action-fmt is installed, then add
# a command suffix that formats errors.
_fmt_clippy := if env_var_or_default("GITHUB_ACTIONS", "") != "true" { "" } else {
    ```
    if command -v cargo-action-fmt >/dev/null 2>&1; then
        echo "--message-format=json -- -Dwarnings | cargo-action-fmt"
    fi
    ```
}

_fmt_check_doc := if env_var_or_default("GITHUB_ACTIONS", "") != "true" { "" } else {
    ```
    if command -v cargo-action-fmt >/dev/null 2>&1; then
        echo "--message-format=json | cargo-action-fmt"
    fi
    ```
}

# default recipe to display help information
default:
    @echo '{{ _docstring }}'
    @just --list


# passes through the LOOM_LOG env var to loom.
export LOOM_LOG := env_var_or_default("LOOM_LOG", "debug")
export LOOM_LOCATION := env_var_or_default("LOOM_LOCATION", "true")
export LOOM_MAX_PREEMPTIONS := env_var_or_default("LOOM_MAX_PREEMPTIONS", "2")
# set default max duration per test to 600 seconds (10 mins)
export LOOM_MAX_DURATION := env_var_or_default("LOOM_MAX_DURATION", "600")
export RUSTDOCFLAGS := env_var_or_default("RUSTDOCFLAGS", "--cfg docsrs")

# passes through MIRIFLAGS to miri
export MIRIFLAGS := env_var_or_default("MIRIFLAGS", "-Zmiri-strict-provenance -Zmiri-disable-isolation")

# run Loom tests
loom *NEXTEST_ARGS="--package tricky-pipe --all-features":
    @echo "LOOM_LOG=${LOOM_LOG}; LOOM_LOCATION=${LOOM_LOCATION}; \
        LOOM_MAX_PREEMPTIONS=${LOOM_MAX_PREEMPTIONS}; \
        LOOM_MAX_DURATION=${LOOM_MAX_DURATION}"
    RUSTFLAGS="--cfg loom --cfg debug_assertions --cfg maitake_ultraverbose" \
        {{ _cargo }} {{ _testcmd }} \
        {{ if no-nextest != "true" { "--profile loom" } else { "" } }} \
         --release {{ NEXTEST_ARGS }}

# run Miri tests
miri *MIRI_ARGS="--package tricky-pipe --all-features --lib":
    @echo "MIRIFLAGS=${MIRIFLAGS}"
    RUSTFLAGS="{{ env_var_or_default("RUSTFLAGS", "-Zrandomize-layout") }}" \
        {{ _rust_log }} {{ _cargo }} miri {{ _testcmd }} \
        {{ if no-nextest != "true" { "--profile miri" } else { "" } }} \
        {{ MIRI_ARGS }}

# run cargo check
check *CARGO_ARGS="--workspace --all-features --all-targets":
    {{ _cargo }} check {{ CARGO_ARGS }} {{ _fmt_check_doc }}

# run cargo clippy
clippy *CLIPPY_ARGS="--workspace --all-features --all-targets":
    {{ _cargo }} clippy {{ CLIPPY_ARGS }} {{ _fmt_check_doc }}

# run cargo test
test *NEXTEST_ARGS="--workspace --all-features":
    {{ _rust_log }} {{ _cargo }} {{ _testcmd }} \
            {{ if env_var_or_default("GITHUB_ACTIONS", "") == "true" { "--profile ci" } else { "" } }} \
            {{ NEXTEST_ARGS }}
    {{ _cargo }} test --doc {{ NEXTEST_ARGS }}

# build RustDoc
docs *RUSTDOC_ARGS="--workspace":
    {{ _cargo }} doc --no-deps --all-features {{ RUSTDOC_ARGS }}