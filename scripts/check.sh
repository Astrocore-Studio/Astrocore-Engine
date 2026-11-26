#!/usr/bin/env bash
# Проверка перед коммитом: формат, clippy без предупреждений, тесты, отсутствие графики в ядре.
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
bash scripts/check-headless.sh
echo "Проверка пройдена."
