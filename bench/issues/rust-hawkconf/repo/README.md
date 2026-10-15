# hawkconf

A tiny parser for `.hawk` config files: `key = value` entries, `#`
comments, blank lines ignored. Ships a small CLI that prints parsed
entries or reports the first error with its line number.

## Usage

    cargo run -- config.hawk

## Tests

    cargo test
