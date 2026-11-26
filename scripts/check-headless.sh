#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
dependencies=$(cargo tree --locked -p astrocore-grid2d --edges normal,build --prefix none)
if printf '%s\n' "$dependencies" | grep -E '^(wgpu|winit|bevy_render|bevy_pbr|bevy_audio)([-_ ]|$)'; then
    echo "2D Engine должен собираться без оконных, графических и аудиозависимостей." >&2
    exit 1
fi
