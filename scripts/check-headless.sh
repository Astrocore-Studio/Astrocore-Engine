#!/usr/bin/env bash
# Проверка границ зависимостей (принципы 1–3 раздела 3 дизайн-документа):
#   1. headless-сервер не тянет окно, GPU, рендер и звук;
#   2. «чистые» доменные крейты не зависят от Bevy;
#   3. запрещённые связи между крейтами Astrocore.
# Запуск: ./scripts/check-headless.sh   (из корня репозитория)
set -euo pipefail

cd "$(dirname "$0")/.."
fail=0

deps() { cargo tree -p "$1" -e normal --prefix none --no-dedupe 2>/dev/null | awk '{print $1}' | sort -u; }

# 1. Сервер и bevy-профиль без графики/окон/звука.
forbidden_runtime='^(wgpu|winit|bevy_render|bevy_window|bevy_winit|bevy_audio|bevy_pbr|bevy_core_pipeline|bevy_sprite|bevy_ui|bevy_gizmos|egui|bevy_egui|cpal|rodio)(-[a-z0-9_]+)?$'
for pkg in astrocore-server astrocore-bevy; do
  bad=$(deps "$pkg" | grep -E "$forbidden_runtime" || true)
  if [[ -n "$bad" ]]; then
    echo "ОШИБКА: $pkg тянет графические/оконные зависимости:"; echo "$bad"; fail=1
  else
    echo "ok: $pkg без wgpu/winit/рендера/звука"
  fi
done

# 2. Чистые доменные крейты не знают про Bevy.
pure=(astrocore-math astrocore-core astrocore-world astrocore-geometry astrocore-spatial
      astrocore-navigation astrocore-sprites astrocore-persistence astrocore-protocol
      astrocore-prefabs astrocore-runtime astrocore-assets astrocore-world-compiler
      astrocore-physics astrocore-network astrocore-editor)
for pkg in "${pure[@]}"; do
  if deps "$pkg" | grep -qE '^bevy(_[a-z0-9_]+)?$'; then
    echo "ОШИБКА: $pkg зависит от Bevy"; fail=1
  else
    echo "ok: $pkg не зависит от Bevy"
  fi
done

# 3. Запрещённые связи: <крейт> -/-> <крейт>.
forbid() {
  local from=$1; shift
  for to in "$@"; do
    if deps "$from" | grep -qx "$to"; then
      echo "ОШИБКА: запрещённая связь $from → $to"; fail=1
    fi
  done
}
forbid astrocore-core    astrocore-world astrocore-geometry astrocore-spatial astrocore-navigation astrocore-bevy astrocore-server
forbid astrocore-world   astrocore-geometry astrocore-spatial astrocore-navigation astrocore-bevy astrocore-server astrocore-protocol astrocore-persistence
forbid astrocore-protocol astrocore-geometry astrocore-sprites astrocore-bevy
forbid astrocore-persistence astrocore-bevy astrocore-protocol
forbid astrocore-math    astrocore-core astrocore-world
forbid astrocore-physics astrocore-network astrocore-protocol astrocore-persistence astrocore-bevy
forbid astrocore-network astrocore-bevy astrocore-persistence astrocore-world-compiler
forbid astrocore-assets  astrocore-world astrocore-geometry astrocore-network
forbid astrocore-runtime astrocore-network astrocore-physics astrocore-protocol astrocore-bevy
echo "ok: запрещённые связи между крейтами отсутствуют"

exit $fail
