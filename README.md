<div class="header" align="center">
<img alt="Astrocore Engine" width="880" height="300" src="docs/img/logo.svg">
</div>

Astrocore Engine — 2D-движок на Rust для сетевых игр по образцу Space Station 14.
Движок переносит [Robust Toolbox](https://github.com/space-wizards/RobustToolbox): сущности и
компоненты, события, прототипы, карты с гридами, репликацию состояний. Отличия от Robust —
этажи Z в ядре, многопоточное расписание и собственный формат контента RON рядом с YAML SS14.
Игра на движке — [Astrocore Build](https://github.com/Astrocore-Studio/Astrocore-Build).

Устройство ядра описано в [ADR-0015](docs/ADR/0015-entity-core.md), план работ — в
[ROADMAP Build](https://github.com/Astrocore-Studio/Astrocore-Build/blob/main/docs/ROADMAP.md).
Список решений: [docs/ADR](docs/ADR/README.md).

## Сборка и проверка

Из корня Engine с установленным Rust stable:

```bash
cargo build --locked --workspace
```

```bash
cargo fmt --all -- --check
```

```bash
cargo clippy --locked --workspace --all-targets -- -D warnings
```

```bash
cargo test --locked --workspace
```

Запуск игры выполняется из соседнего чекаута Build командой `cargo play`.

## Лицензия

Astrocore Engine распространяется по лицензии
[GNU Affero General Public License v3.0 или новее](LICENSE) (`AGPL-3.0-or-later`).

Если вы распространяете движок или игру на нём либо запускаете изменённый движок как сетевой
сервис, AGPL обязывает предоставить исходный код пользователям, включая ваши изменения.
