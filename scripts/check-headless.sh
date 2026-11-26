#!/usr/bin/env bash
# Серверные крейты движка собираются без оконных, графических и аудиозависимостей.
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
for package in astrocore-data astrocore-math astrocore-ecs astrocore-prototypes astrocore-map astrocore-console astrocore-grid2d; do
    dependencies=$(cargo tree --locked -p "$package" --edges normal,build --prefix none)
    if printf '%s\n' "$dependencies" | grep -E '^(wgpu|winit|bevy_render|bevy_pbr|bevy_audio)([-_ ]|$)'; then
        echo "$package должен собираться без оконных, графических и аудиозависимостей." >&2
        exit 1
    fi
done
