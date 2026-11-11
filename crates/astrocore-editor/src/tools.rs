//! Инструменты построения. Каждый инструмент **планирует** изменение: читает мир и
//! возвращает список команд, не меняя мир. Применяет их [`EditSession::execute`]
//! как одну обратимую транзакцию.
//!
//! Автоматика (то, что делает редактор удобным, а не просто «рисователем боксов»):
//! замкнутый контур → комната с полом и потолком; лестница сама пробивает
//! перекрытия над собой; копирование, поворот и зеркало областей.

use std::collections::BTreeSet;

use astrocore_core::{ObjectId, PrototypeId};
use astrocore_math::{Facing, GridCell, GridEdge, GridTransform};
use astrocore_spatial::{RoomKind, Topology};
use astrocore_world::{
    FloorDef, FloorId, Placement, SlotKey, StructureElement, StructureKind, World, WorldCommand,
};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ToolError {
    #[error("этаж {0:?} не определён")]
    UnknownFloor(FloorId),
    #[error("отрезок ({0}, {1}) → ({2}, {3}) не параллелен осям сетки")]
    NotAxisAligned(i32, i32, i32, i32),
    #[error("ребро {0:?} занято проёмом — стену поверх не построить")]
    EdgeHasOpening(GridEdge),
    #[error("место уже занято: {0}")]
    Occupied(String),
    #[error("нечего делать: результат пуст")]
    Empty,
    #[error("в выделении нет элементов")]
    EmptySelection,
    #[error("ограничение: {0}")]
    Limit(String),
    #[error("префаб: {0}")]
    Prefab(String),
}

/// Верхняя граница размера одной операции (защита интерфейса от зависания).
pub const MAX_TOOL_ELEMENTS: usize = 100_000;

pub trait IdGen {
    fn next_id(&mut self) -> ObjectId;
}

/// Случайные идентификаторы (рабочий режим).
pub struct RandomIds;

impl IdGen for RandomIds {
    fn next_id(&mut self) -> ObjectId {
        ObjectId::new_random()
    }
}

/// Последовательные идентификаторы — для тестов и воспроизводимых сценариев.
pub struct SequentialIds(pub u128);

impl IdGen for SequentialIds {
    fn next_id(&mut self) -> ObjectId {
        self.0 += 1;
        ObjectId::from_u128(self.0)
    }
}

fn check_floor(world: &World, floor: FloorId) -> Result<(), ToolError> {
    world
        .floor(floor)
        .map(|_| ())
        .ok_or(ToolError::UnknownFloor(floor))
}

fn element(
    ids: &mut dyn IdGen,
    kind: StructureKind,
    placement: Placement,
    floor: FloorId,
    material: &PrototypeId,
    thickness: f32,
) -> WorldCommand {
    WorldCommand::PlaceElement(StructureElement {
        id: ids.next_id(),
        kind,
        placement,
        floor,
        material: material.clone(),
        thickness,
    })
}

/// Стены вдоль ломаной по вершинам сетки. Уже существующие стены пропускаются,
/// повторное применение ничего не добавляет.
pub fn wall_path(
    world: &World,
    ids: &mut dyn IdGen,
    floor: FloorId,
    vertices: &[(i32, i32)],
    material: &PrototypeId,
    thickness: f32,
) -> Result<Vec<WorldCommand>, ToolError> {
    check_floor(world, floor)?;
    let mut edges: Vec<GridEdge> = Vec::new();
    for pair in vertices.windows(2) {
        let ((x0, z0), (x1, z1)) = (pair[0], pair[1]);
        if x0 != x1 && z0 != z1 {
            return Err(ToolError::NotAxisAligned(x0, z0, x1, z1));
        }
        if (x1 - x0).abs() + (z1 - z0).abs() > MAX_TOOL_ELEMENTS as i32 {
            return Err(ToolError::Limit("слишком длинная линия".into()));
        }
        if x0 != x1 {
            for x in x0.min(x1)..x0.max(x1) {
                edges.push(GridEdge::along_x(x, z0));
            }
        } else {
            for z in z0.min(z1)..z0.max(z1) {
                edges.push(GridEdge::along_z(x0, z));
            }
        }
    }
    let mut seen = BTreeSet::new();
    let mut cmds = Vec::new();
    for edge in edges {
        if !seen.insert(edge) {
            continue;
        }
        match world.edge_element(floor, edge) {
            Some(e) if matches!(e.kind, StructureKind::Wall) => continue,
            Some(_) => return Err(ToolError::EdgeHasOpening(edge)),
            None => cmds.push(element(
                ids,
                StructureKind::Wall,
                Placement::Edge(edge),
                floor,
                material,
                thickness,
            )),
        }
    }
    Ok(cmds)
}

/// Прямоугольная комната по клеткам `min..=max`: стены по периметру и, по желанию, пол и потолок.
#[allow(clippy::too_many_arguments)]
pub fn room_rect(
    world: &World,
    ids: &mut dyn IdGen,
    floor: FloorId,
    min: GridCell,
    max: GridCell,
    material: &PrototypeId,
    thickness: f32,
    with_floor: bool,
    with_ceiling: bool,
) -> Result<Vec<WorldCommand>, ToolError> {
    let (lo, hi) = (
        GridCell::new(min.x.min(max.x), min.z.min(max.z)),
        GridCell::new(min.x.max(max.x), min.z.max(max.z)),
    );
    let area = ((hi.x - lo.x + 1) as i64) * ((hi.z - lo.z + 1) as i64);
    if area > MAX_TOOL_ELEMENTS as i64 {
        return Err(ToolError::Limit(format!("площадь {area} клеток")));
    }
    let ring = [
        (lo.x, lo.z),
        (hi.x + 1, lo.z),
        (hi.x + 1, hi.z + 1),
        (lo.x, hi.z + 1),
        (lo.x, lo.z),
    ];
    let mut cmds = wall_path(world, ids, floor, &ring, material, thickness)?;
    for kind in [
        (with_floor, StructureKind::Floor),
        (with_ceiling, StructureKind::Ceiling),
    ] {
        if !kind.0 {
            continue;
        }
        for x in lo.x..=hi.x {
            for z in lo.z..=hi.z {
                let cell = GridCell::new(x, z);
                let key = if matches!(kind.1, StructureKind::Floor) {
                    SlotKey::FloorSlab(cell)
                } else {
                    SlotKey::CeilingSlab(cell)
                };
                if world.element_at(floor, key).is_none() {
                    cmds.push(element(
                        ids,
                        kind.1,
                        Placement::Cell(cell),
                        floor,
                        material,
                        thickness,
                    ));
                }
            }
        }
    }
    Ok(cmds)
}

/// Автоматические пол или потолок во всех замкнутых помещениях этажа, где их ещё нет.
/// Помещение определяется по стенам (топология), поэтому для замкнутого контура
/// достаточно одного вызова.
pub fn auto_slabs(
    world: &World,
    ids: &mut dyn IdGen,
    floor: FloorId,
    kind: StructureKind,
    material: &PrototypeId,
    thickness: f32,
) -> Result<Vec<WorldCommand>, ToolError> {
    if !matches!(kind, StructureKind::Floor | StructureKind::Ceiling) {
        return Err(ToolError::Occupied(
            "автоматически строятся только полы и потолки".into(),
        ));
    }
    let topo = Topology::build(world, floor).map_err(|_| ToolError::UnknownFloor(floor))?;
    let mut cmds = Vec::new();
    for room in topo.rooms.iter().filter(|r| r.kind == RoomKind::Interior) {
        for &cell in &room.cells {
            let key = if matches!(kind, StructureKind::Floor) {
                SlotKey::FloorSlab(cell)
            } else {
                SlotKey::CeilingSlab(cell)
            };
            if world.element_at(floor, key).is_none() {
                cmds.push(element(
                    ids,
                    kind,
                    Placement::Cell(cell),
                    floor,
                    material,
                    thickness,
                ));
            }
        }
    }
    if cmds.len() > MAX_TOOL_ELEMENTS {
        return Err(ToolError::Limit("слишком много клеток".into()));
    }
    if cmds.is_empty() {
        Err(ToolError::Empty)
    } else {
        Ok(cmds)
    }
}

/// Лестница или пандус между этажами. При `pierce` над маршем убираются перекрытия
/// верхнего этажа (пол) и потолок нижнего, чтобы не оставлять «крышку» над лестницей.
#[allow(clippy::too_many_arguments)]
pub fn stair_between(
    world: &World,
    ids: &mut dyn IdGen,
    floor: FloorId,
    to: FloorId,
    start: GridCell,
    dir: Facing,
    length: u8,
    ramp: bool,
    material: &PrototypeId,
    pierce: bool,
) -> Result<Vec<WorldCommand>, ToolError> {
    check_floor(world, floor)?;
    check_floor(world, to)?;
    let kind = if ramp {
        StructureKind::Ramp { to }
    } else {
        StructureKind::Stair { to }
    };
    let placement = Placement::Run { start, dir, length };
    let probe = StructureElement {
        id: ObjectId::from_u128(0),
        kind,
        placement,
        floor,
        material: material.clone(),
        thickness: 0.2,
    };
    for slot in probe.slots() {
        if let Some(by) = world.element_at(slot.floor, slot.key) {
            return Err(ToolError::Occupied(format!(
                "клетка марша занята объектом {}",
                by.id
            )));
        }
    }
    let mut cmds = Vec::new();
    if pierce {
        for cell in placement.run_cells() {
            for (fl, key) in [
                (to, SlotKey::FloorSlab(cell)),
                (floor, SlotKey::CeilingSlab(cell)),
            ] {
                if let Some(e) = world.element_at(fl, key) {
                    cmds.push(WorldCommand::RemoveElement { id: e.id });
                }
            }
        }
    }
    cmds.push(element(ids, kind, placement, floor, material, 0.2));
    Ok(cmds)
}

/// Новый этаж над самым высоким: возвращает команду и идентификатор.
pub fn add_floor_above(world: &World, name: &str, height: f32) -> (FloorId, WorldCommand) {
    let top = world
        .floors()
        .max_by(|a, b| a.top_y().total_cmp(&b.top_y()));
    let (id, base_y) = top.map_or((FloorId(0), 0.0), |f| (FloorId(f.id.0 + 1), f.top_y()));
    (
        id,
        WorldCommand::DefineFloor(FloorDef {
            id,
            name: name.to_string(),
            base_y,
            height,
        }),
    )
}

// ---- выделение и операции над ним -------------------------------------------

/// Элементы этажа, целиком лежащие внутри прямоугольника клеток `min..=max`.
pub fn select_rect(
    world: &World,
    floor: FloorId,
    min: GridCell,
    max: GridCell,
) -> BTreeSet<ObjectId> {
    let (lo, hi) = (
        GridCell::new(min.x.min(max.x), min.z.min(max.z)),
        GridCell::new(min.x.max(max.x), min.z.max(max.z)),
    );
    world
        .elements()
        .filter(|e| e.floor == floor)
        .filter(|e| {
            e.placement.touched_cells().iter().all(|c| c.x >= lo.x && c.x <= hi.x && c.z >= lo.z && c.z <= hi.z)
                // Ребро на границе прямоугольника касается ячейки снаружи — считаем его внутри, если одна из сторон внутри.
                || e.placement.touched_cells().iter().any(|c| c.x >= lo.x && c.x <= hi.x && c.z >= lo.z && c.z <= hi.z)
                    && matches!(e.placement, Placement::Edge(_))
        })
        .map(|e| e.id)
        .collect()
}

pub fn delete(
    world: &World,
    selection: &BTreeSet<ObjectId>,
) -> Result<Vec<WorldCommand>, ToolError> {
    let cmds: Vec<_> = selection
        .iter()
        .filter(|id| world.element(**id).is_some())
        .map(|id| WorldCommand::RemoveElement { id: *id })
        .collect();
    if cmds.is_empty() {
        Err(ToolError::EmptySelection)
    } else {
        Ok(cmds)
    }
}

fn transform_placement(p: Placement, t: GridTransform, step: f32) -> Placement {
    match p {
        Placement::Edge(e) => Placement::Edge(t.edge(e)),
        Placement::Cell(c) => Placement::Cell(t.cell(c)),
        Placement::Run { start, dir, length } => {
            let (start, dir) = t.run(start, dir);
            Placement::Run { start, dir, length }
        }
        Placement::Free(f) => {
            let pos = f.position(step);
            let (x, z) = t.point((pos[0] / step, pos[2] / step));
            let moved = [x * step, pos[1], z * step];
            let half = f.half_mm.map(|h| h as f32 / 1000.0);
            // Полуразмеры задаются в осях модели, поэтому при повороте не меняются.
            let mut out = astrocore_world::FreePlacement::from_position(moved, step, 0.0, half);
            out.yaw_cdeg = t.yaw_cdeg(f.yaw_cdeg);
            out.y_mm = f.y_mm;
            Placement::Free(out)
        }
    }
}

/// Проверка, что все места, куда встанут образы элементов, свободны (с учётом `vacated`).
fn check_destination(
    world: &World,
    images: &[StructureElement],
    vacated: &BTreeSet<ObjectId>,
) -> Result<(), ToolError> {
    let mut claimed = BTreeSet::new();
    for e in images {
        for slot in e.slots() {
            if !claimed.insert(slot) {
                return Err(ToolError::Occupied(format!(
                    "два образа претендуют на {slot:?}"
                )));
            }
            if let Some(other) = world.element_at(slot.floor, slot.key)
                && !vacated.contains(&other.id)
            {
                return Err(ToolError::Occupied(format!(
                    "{slot:?} занято объектом {}",
                    other.id
                )));
            }
        }
    }
    Ok(())
}

/// Копия выделения с преобразованием (поворот, отражение, перенос) и сдвигом этажей.
/// Копии получают новые идентификаторы.
pub fn copy_selection(
    world: &World,
    ids: &mut dyn IdGen,
    selection: &BTreeSet<ObjectId>,
    transform: GridTransform,
    floor_shift: i32,
) -> Result<Vec<WorldCommand>, ToolError> {
    let images = images_of(world, selection, transform, floor_shift, |_| ids.next_id())?;
    check_destination(world, &images, &BTreeSet::new())?;
    Ok(images.into_iter().map(WorldCommand::PlaceElement).collect())
}

/// Перемещение выделения с сохранением идентификаторов: сначала удаление, затем размещение.
pub fn move_selection(
    world: &World,
    selection: &BTreeSet<ObjectId>,
    transform: GridTransform,
    floor_shift: i32,
) -> Result<Vec<WorldCommand>, ToolError> {
    let images = images_of(world, selection, transform, floor_shift, |e| e.id)?;
    check_destination(world, &images, selection)?;
    let mut cmds: Vec<_> = selection
        .iter()
        .filter(|id| world.element(**id).is_some())
        .map(|id| WorldCommand::RemoveElement { id: *id })
        .collect();
    cmds.extend(images.into_iter().map(WorldCommand::PlaceElement));
    Ok(cmds)
}

fn images_of(
    world: &World,
    selection: &BTreeSet<ObjectId>,
    t: GridTransform,
    floor_shift: i32,
    mut new_id: impl FnMut(&StructureElement) -> ObjectId,
) -> Result<Vec<StructureElement>, ToolError> {
    if selection.len() > MAX_TOOL_ELEMENTS {
        return Err(ToolError::Limit(format!(
            "{} элементов в выделении",
            selection.len()
        )));
    }
    let mut out = Vec::new();
    for id in selection {
        let Some(e) = world.element(*id) else {
            continue;
        };
        let floor = FloorId(e.floor.0 + floor_shift);
        check_floor(world, floor)?;
        let mut kind = e.kind;
        if let Some(to) = kind.target_floor() {
            let to = FloorId(to.0 + floor_shift);
            check_floor(world, to)?;
            kind = match kind {
                StructureKind::Stair { .. } => StructureKind::Stair { to },
                _ => StructureKind::Ramp { to },
            };
        }
        out.push(StructureElement {
            id: new_id(e),
            kind,
            placement: transform_placement(e.placement, t, world.config().grid_step_m),
            floor,
            ..e.clone()
        });
    }
    if out.is_empty() {
        Err(ToolError::EmptySelection)
    } else {
        Ok(out)
    }
}

/// Массовая замена материала (элементы пересоздаются с теми же идентификаторами).
pub fn replace_material(
    world: &World,
    selection: &BTreeSet<ObjectId>,
    material: &PrototypeId,
) -> Result<Vec<WorldCommand>, ToolError> {
    let mut cmds = Vec::new();
    let mut places = Vec::new();
    for id in selection {
        let Some(e) = world.element(*id) else {
            continue;
        };
        if &e.material == material {
            continue;
        }
        cmds.push(WorldCommand::RemoveElement { id: e.id });
        places.push(WorldCommand::PlaceElement(StructureElement {
            material: material.clone(),
            ..e.clone()
        }));
    }
    cmds.extend(places);
    if cmds.is_empty() {
        Err(ToolError::Empty)
    } else {
        Ok(cmds)
    }
}

// ---- префабы ----------------------------------------------------------------

/// Результат размещения или обновления экземпляра префаба.
pub struct PrefabPlan {
    pub commands: Vec<WorldCommand>,
    /// Объекты экземпляра после применения; сохраняются в проекте для следующих обновлений.
    pub spawned: BTreeSet<ObjectId>,
    pub conflicts: Vec<astrocore_prefabs::Conflict>,
}

pub fn place_prefab(
    registry: &astrocore_prefabs::PrefabRegistry,
    instance: &astrocore_prefabs::PrefabInstance,
    previously_spawned: &BTreeSet<ObjectId>,
    world: &World,
) -> Result<PrefabPlan, ToolError> {
    let r = astrocore_prefabs::reconcile(registry, instance, previously_spawned, world)
        .map_err(|e| ToolError::Prefab(e.to_string()))?;
    Ok(PrefabPlan {
        commands: r.commands,
        spawned: r.spawned,
        conflicts: r.conflicts,
    })
}
