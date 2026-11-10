# Архитектура

Источник: дизайн-документ Astrocore Engine v1.2, разделы 3–8. Здесь — то, что
реально реализовано в репозитории.

## Принципы

1. Движок не импортирует код и данные игровых билдов.
2. Каноническая модель мира не зависит от рендера, GPU и клиента.
3. Сервер не создаёт графический контекст и не зависит от оконной системы.
4. Любое изменение мира — валидируемая команда, порождающая ревизию.
5. Форматы сохранения не содержат индексов ECS.
6. Каждая подсистема имеет headless-тесты и публичный контракт.

Пункты 2–3 проверяются автоматически: `scripts/check-headless.sh` (запускается в CI).

## Крейты и направление зависимостей

```
astrocore-math ─┐
                ├─► astrocore-world ─┬─► astrocore-geometry
astrocore-core ─┘                    ├─► astrocore-spatial
                                     ├─► astrocore-navigation
                                     ├─► astrocore-persistence
                                     ├─► astrocore-protocol
                                     └─► astrocore-bevy ─► astrocore-server

astrocore-math ─► astrocore-sprites         (независим от мира)
```

«Чистые» доменные крейты (все, кроме `astrocore-bevy` и `astrocore-server`) не
зависят от Bevy. Bevy используется только как адаптер ECS/расписания.

Запрещённые связи (проверяются скриптом): `core → world/geometry/…`,
`world → geometry/spatial/navigation/bevy/server/protocol/persistence`,
`protocol → geometry/sprites/bevy`, `persistence → bevy/protocol`,
`math → core/world`, а также любой графический стек в `server` и `bevy`.

## Жизненный цикл изменения мира

1. Источник (редактор, сетевая команда, скрипт) формирует `WorldCommand`.
2. `World::apply` валидирует команду целиком **до** мутации; при ошибке мир не меняется.
3. Ревизия мира увеличивается; возвращается `WorldOp { seq, command }`, где
   `seq` = ревизия после применения.
4. Затронутые чанки×этажи помечаются dirty с флагами `GEOMETRY / COLLISION /
   NAVIGATION / TOPOLOGY`; ревизии затронутых чанков обновляются.
5. Потребители забирают `take_dirty()` и пересобирают производные данные.
6. `WorldOp` записывается в журнал (`astrocore-persistence`) и рассылается клиентам
   (`astrocore-protocol`); клиенты применяют через `apply_replicated` строго по порядку.

## Расписание (astrocore-bevy)

Фиксированный шаг (по умолчанию 20 Гц, настраивается), стадии:

`Input → Commands → Simulation → WorldChanges → Replication → Presentation`

Порядок объявлен через `SimSet` и проверяется тестом. Частота тика отделена от
частоты кадров (тест `fixed_step_is_independent_of_frame_rate`).

## Идентичность

`ObjectId` (UUID) — постоянный; `Entity` Bevy — только runtime, в файлы и сеть не
попадает; связь хранит `ObjectIndex`. Подробнее — ADR-0004.
