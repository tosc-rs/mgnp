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
    LOOM_LOG='debug'        # sets the log level for Loom tests.
    LOOM_LOCATION='true'    # enables/disables location tracking in Loom tests.

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


export LOOM_LOG := env_var_or_default("LOOM_LOG", "debug")
export LOOM_LOCATION := env_var_or_default("LOOM_LOCATION", "true")
export LOOM_MAX_PREEMPTIONS := env_var_or_default("LOOM_MAX_PREEMPTIONS", "2")

# run Loom tests
loom *ARGS:
    RUSTFLAGS=" --cfg loom --cfg debug_assertions" \
        {{ _cargo }} {{ _testcmd }} --release {{ ARGS }}
