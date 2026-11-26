<div class="header" align="center">
<img alt="Astrocore Engine" width="880" height="300" src="docs/img/logo.svg">
</div>

Astrocore Engine — 2D-движок на Rust для сетевой станционной песочницы.
`astrocore-grid2d` предоставляет тайловую сетку, коллизии и авторитарный
сетевой сеанс через Lightyear. Игровые правила, графический клиент и
редактор карт находятся в [Astrocore Build](https://github.com/Astrocore-Studio/Astrocore-Build).

## Сборка и проверка

Из корня Engine с установленным Rust stable:

```bash
cargo build --locked --workspace
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

Запуск игры выполняется из соседнего чекаута Build командой `cargo play`.
В workspace Engine входят 2D-ядро и библиотеки, от которых оно зависит.
[ADR перехода на 2D](docs/ADR/0014-native-2d-grid.md)

## Лицензия

Astrocore Engine распространяется по лицензии
[GNU Affero General Public License v3.0 или новее](LICENSE) (`AGPL-3.0-or-later`).

Если вы распространяете движок или игру на нём либо запускаете изменённый движок как сетевой
сервис, AGPL обязывает предоставить исходный код пользователям, включая ваши изменения.

Ассеты, которые использует движок в демо и тестах, распространяются по лицензиям, указанным в их
файлах метаданных; подробности — в [docs/ASSETS.md](docs/ASSETS.md).
