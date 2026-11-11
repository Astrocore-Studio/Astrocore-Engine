#!/usr/bin/env bash
# Дымовая проверка рендера без GPU и без экрана: клиент на программном Vulkan (lavapipe)
# под виртуальным дисплеем делает скриншот демо-сцены; пустой (чёрный) кадр — провал.
# Нужны пакеты: xvfb mesa-vulkan-drivers libxkbcommon-x11-0
set -euo pipefail
cd "$(dirname "$0")/.."

out="${1:-target/render-smoke.png}"
mkdir -p "$(dirname "$out")"
rm -f "$out"

export VK_ICD_FILENAMES="${VK_ICD_FILENAMES:-/usr/share/vulkan/icd.d/lvp_icd.json}"
cargo build -p astrocore-client
timeout 240 xvfb-run -a -s "-screen 0 1280x720x24" \
  ./target/debug/astrocore-client --demo --screenshot "$out" --frames 90 >/dev/null 2>&1 || true

[[ -f "$out" ]] || { echo "ОШИБКА: скриншот не создан"; exit 1; }
size=$(stat -c %s "$out")
# Однотонный кадр (чёрный или только фон) сжимается в единицы килобайт; сцена со зданием, землёй
# и спрайтами — в сотни.
if (( size < 60000 )); then
  echo "ОШИБКА: кадр похож на пустой ($size байт)"; exit 1
fi
echo "ok: рендер работает, кадр $out ($size байт)"
