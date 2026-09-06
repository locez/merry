#!/usr/bin/env bash
set -euo pipefail

listing=$(cargo test --locked "$@" -- --list)
if ! grep -q ': test$' <<< "$listing"; then
    printf 'No tests matched scenario: %s\n' "$*" >&2
    exit 1
fi
cargo test --locked "$@" -- --nocapture
