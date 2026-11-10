# Astrocore Engine

Универсальный игровой движок на Rust (поверх [Bevy](https://bevyengine.org)) для
многопользовательских 2.5D/3D-песочниц: физически трёхмерный мир, структурная сетка
и свободная геометрия, 2D-спрайты (directional billboards) в 3D-сцене,
сервер-авторитарное состояние, графический редактор карт (Astrocore Studio).

> **Репозиторий содержит только движок.** Игровые билды, контент, карты, правила и
> сеттинги живут в отдельных репозиториях и подключают движок как зависимость.

Проектный документ: страница «Astrocore Engine — Архитектура гибридного 3D-мира и
редактора карт» в Notion (версия 1.2). Принятые решения зафиксированы в
[`docs/ADR`](docs/ADR), фактическое состояние — в [`docs/PROGRESS.md`](docs/PROGRESS.md).

## Что уже есть

Реализована и проверена тестами **headless-основа** (фаза F0 и значимая часть F2–F4):

| Крейт | Назначение |
| --- | --- |
| `astrocore-math` | Конвенция координат (метры, **Y-up**, правосторонняя), чанки, ячейки и рёбра сетки |
| `astrocore-core` | Идентификаторы (`ObjectId`, `PrototypeId`, …), ревизии, версии схем |
| `astrocore-world` | Канонический мир: структурные элементы, команды, ревизии, dirty-регионы, репликация операций |
| `astrocore-geometry` | Меши стен и перекрытий, батчинг по материалам (без GPU) |
| `astrocore-spatial` | Топология: комнаты, порталы с независимыми каналами, диагностика карты |
| `astrocore-navigation` | Запросы пути по каноническому миру |
| `astrocore-sprites` | Выбор направления billboard-спрайта по камере; чтение метаданных RSI |
| `astrocore-persistence` | Снимок + журнал с CRC, атомарная запись, восстановление, миграции |
| `astrocore-protocol` | Версионированный бинарный протокол с проверкой недоверенного ввода |
| `astrocore-bevy` | Bevy-плагины: расписание, headless-симуляция, связь мира с ECS |
| `astrocore-server` | Headless-сервер (без окна, GPU и звука) |
| `astrocore-vertical-slice` | Приёмочные проверки AC-VS1 (headless-часть) |

**Ещё нет:** клиент с окном и рендером, физика (Avian), сетевой транспорт (Lightyear),
Astrocore Studio. Подробности и причины — в [`docs/PROGRESS.md`](docs/PROGRESS.md).

## Быстрый старт

Нужен Rust stable (проверено на 1.99.1; Bevy 0.20 требует не ниже 1.97.1).

```bash
cargo test --workspace
```

```bash
./scripts/check-headless.sh
```

```bash
cargo run -p astrocore-vertical-slice
```

```bash
cargo run -p astrocore-server -- --data-dir ./data --ticks 100
```

Перед отправкой изменений (правило 4 раздела 26 дизайн-документа):

```bash
cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

## Документация

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — крейты, зависимости, жизненный цикл
- [`docs/WORLD_FORMAT.md`](docs/WORLD_FORMAT.md) — мир, координаты, ревизии, идентификаторы
- [`docs/PERSISTENCE.md`](docs/PERSISTENCE.md) — снимки, журнал, миграции
- [`docs/NETWORKING.md`](docs/NETWORKING.md) — протокол и репликация
- [`docs/RENDERING.md`](docs/RENDERING.md) — режимы отображения, спрайты (проект)
- [`docs/EDITOR.md`](docs/EDITOR.md) — Astrocore Studio (проект)
- [`docs/ASSETS.md`](docs/ASSETS.md) — ассеты, RSI, лицензии
- [`docs/TEST_PLAN.md`](docs/TEST_PLAN.md) — тесты и приёмка
- [`docs/ADR`](docs/ADR) — архитектурные решения

## Лицензия

Лицензия репозитория **ещё не выбрана** (решение владельца проекта). Пока файла
`LICENSE` нет, крейты помечены `publish = false`.
