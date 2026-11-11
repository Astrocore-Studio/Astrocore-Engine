use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use astrocore_core::{ObjectId, PrototypeId};
use astrocore_editor::EditSession;
use astrocore_editor::diff::{WorldDiff, diff_worlds};
use astrocore_editor::gizmo::{Axis, AxisDrag, Ray, pick_axis, snap, yaw_toward};
use astrocore_editor::project::{InstanceRecord, Project};
use astrocore_editor::snap::{nearest_edge, snap_cell, snap_vertex};
use astrocore_editor::tools::{self, IdGen, RandomIds, ToolError};
use astrocore_editor::validate::{Finding, Severity, has_errors, validate_map};
use astrocore_editor::view::FloorView;
use astrocore_math::glam::Vec3;

/// Фильтр видимости элементов для рендера.
pub type ElementFilter = Arc<dyn Fn(&StructureElement) -> bool + Send + Sync>;
use astrocore_math::{Facing, GridCell, GridTransform};
use astrocore_prefabs::{PrefabInstance, PrefabRegistry};
use astrocore_world::{
    FloorDef, FloorId, OpeningKind, Placement, SlotKey, StructureElement, StructureKind, World,
    WorldCommand, WorldConfig,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Select,
    WallLine,
    Room,
    Door,
    Stair,
    Model,
    Delete,
}

impl Tool {
    pub const ALL: [Tool; 7] = [
        Tool::Select,
        Tool::WallLine,
        Tool::Room,
        Tool::Door,
        Tool::Stair,
        Tool::Model,
        Tool::Delete,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Tool::Select => "Выбор",
            Tool::WallLine => "Стены (линия)",
            Tool::Room => "Комната",
            Tool::Door => "Дверь",
            Tool::Stair => "Лестница",
            Tool::Model => "Модель",
            Tool::Delete => "Удалить",
        }
    }
}

/// События указателя; точки — на плоскости текущего этажа (мировые координаты, метры).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pointer {
    Press(Vec3),
    Drag(Vec3),
    Release(Vec3),
}

/// Подсказка для отрисовки «призрака» операции.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Preview {
    None,
    /// Отрезок между вершинами сетки (в метрах, на плоскости этажа).
    Line(Vec3, Vec3),
    /// Прямоугольник клеток: углы в метрах.
    Rect(Vec3, Vec3),
}

pub struct StudioState {
    pub session: EditSession,
    ids: Box<dyn IdGen>,
    pub tool: Tool,
    pub floor: FloorId,
    pub material: String,
    pub thickness: f32,
    pub room_floor: bool,
    pub room_ceiling: bool,
    pub stair_dir: Facing,
    pub stair_len: u8,
    pub stair_ramp: bool,
    pub view: FloorView,
    pub selection: BTreeSet<ObjectId>,
    pub additive_select: bool,
    pub status: String,
    pub findings: Vec<Finding>,
    pub project_path: Option<PathBuf>,
    anchor: Option<Vec3>,
    pub preview: Preview,
    /// Идентификатор модели для инструмента «Модель» (`ns:имя`; файл `ns_имя.glb`).
    pub model: String,
    pub model_half: [f32; 3],
    pub prefabs: PrefabRegistry,
    pub instances: Vec<InstanceRecord>,
    /// Консоль: журнал действий и сообщений (последние `CONSOLE_LIMIT` строк).
    pub console: Vec<String>,
    /// Мир на момент открытия/сохранения — основа панели различий.
    baseline: Option<World>,
    gizmo: Option<AxisDrag>,
    /// Шаг привязки перетаскивания гизмо, м (0 — без привязки).
    pub gizmo_snap: f32,
    gizmo_target: Option<StructureElement>,
    /// Растёт при каждом изменении мира или режима просмотра — оболочка по нему обновляет рендер.
    pub stamp: u64,
}

/// Сколько строк консоли хранится.
pub const CONSOLE_LIMIT: usize = 500;

/// Длина осей гизмо, м.
pub const GIZMO_LENGTH: f32 = 2.0;

/// Поля свободной модели для панели свойств.
#[derive(Debug, Clone, PartialEq)]
pub struct FreeFields {
    pub position: [f32; 3],
    pub yaw_deg: f32,
    pub half_m: [f32; 3],
}

/// Редактируемые поля выбранного элемента.
#[derive(Debug, Clone, PartialEq)]
pub struct Inspector {
    pub id: ObjectId,
    pub kind: String,
    pub material: String,
    pub thickness: f32,
    pub free: Option<FreeFields>,
}

fn new_world() -> World {
    let mut w =
        World::new("studio", WorldConfig::default()).expect("конфигурация по умолчанию верна");
    w.apply(WorldCommand::DefineFloor(FloorDef {
        id: FloorId(0),
        name: "Этаж 0".into(),
        base_y: 0.0,
        height: 3.0,
    }))
    .expect("первый этаж");
    w
}

impl StudioState {
    pub fn new() -> Self {
        Self::with_world(new_world(), Box::new(RandomIds))
    }

    pub fn with_world(world: World, ids: Box<dyn IdGen>) -> Self {
        let floor = world.floors().next().map_or(FloorId(0), |f| f.id);
        Self {
            session: EditSession::new(world),
            ids,
            tool: Tool::WallLine,
            floor,
            material: "studio:wall".into(),
            thickness: 0.2,
            room_floor: true,
            room_ceiling: false,
            stair_dir: Facing::East,
            stair_len: 4,
            stair_ramp: false,
            view: FloorView::All,
            selection: BTreeSet::new(),
            additive_select: false,
            status: "Готово".into(),
            findings: Vec::new(),
            project_path: None,
            anchor: None,
            preview: Preview::None,
            model: "studio:model".into(),
            model_half: [0.5, 1.0, 0.5],
            prefabs: PrefabRegistry::new(),
            instances: Vec::new(),
            console: vec!["Studio запущена".into()],
            baseline: None,
            gizmo: None,
            gizmo_snap: 0.25,
            gizmo_target: None,
            stamp: 1,
        }
    }

    pub fn world(&self) -> &World {
        self.session.world()
    }

    pub fn floor_base_y(&self) -> f32 {
        self.world().floor(self.floor).map_or(0.0, |f| f.base_y)
    }

    fn step(&self) -> f32 {
        self.world().config().grid_step_m
    }

    /// Показать сообщение в строке состояния и записать в консоль.
    pub fn say(&mut self, msg: String) {
        self.console.push(msg.clone());
        if self.console.len() > CONSOLE_LIMIT {
            let extra = self.console.len() - CONSOLE_LIMIT;
            self.console.drain(..extra);
        }
        self.status = msg;
    }

    fn material_id(&self) -> Result<PrototypeId, String> {
        PrototypeId::new(self.material.trim()).map_err(|e| format!("материал: {e}"))
    }

    fn report(&mut self, label: &str, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.say(format!("Выполнено: {label}"));
                self.stamp += 1;
                self.selection.retain(|id| self.session.exists(*id));
            }
            Err(e) => self.say(format!("Не получилось: {e}")),
        }
    }

    fn run(&mut self, label: &str, plan: Result<Vec<WorldCommand>, ToolError>) {
        let result = plan
            .map_err(|e| e.to_string())
            .and_then(|cmds| self.session.execute(label, cmds).map_err(|e| e.to_string()));
        self.report(label, result);
    }

    // ---- указатель ------------------------------------------------------------

    pub fn pointer(&mut self, ev: Pointer) {
        match ev {
            Pointer::Press(p) => {
                self.anchor = Some(p);
                self.preview = Preview::None;
            }
            Pointer::Drag(p) => self.update_preview(p),
            Pointer::Release(p) => {
                if let Some(a) = self.anchor.take() {
                    self.finish(a, p);
                }
                self.preview = Preview::None;
            }
        }
    }

    pub fn cancel(&mut self) {
        self.anchor = None;
        self.preview = Preview::None;
    }

    fn vertex_point(&self, v: (i32, i32)) -> Vec3 {
        Vec3::new(
            v.0 as f32 * self.step(),
            self.floor_base_y(),
            v.1 as f32 * self.step(),
        )
    }

    /// Вершина конца линии: ось выбирается по большему смещению (стены только вдоль осей).
    fn line_end(&self, a: Vec3, b: Vec3) -> ((i32, i32), (i32, i32)) {
        let va = snap_vertex(a, self.step());
        let mut vb = snap_vertex(b, self.step());
        if (vb.0 - va.0).abs() >= (vb.1 - va.1).abs() {
            vb.1 = va.1;
        } else {
            vb.0 = va.0;
        }
        (va, vb)
    }

    fn update_preview(&mut self, p: Vec3) {
        let Some(a) = self.anchor else { return };
        self.preview = match self.tool {
            Tool::WallLine => {
                let (va, vb) = self.line_end(a, p);
                Preview::Line(self.vertex_point(va), self.vertex_point(vb))
            }
            Tool::Room | Tool::Select => {
                let (ca, cb) = (snap_cell(a, self.step()), snap_cell(p, self.step()));
                let (lo, hi) = (
                    GridCell::new(ca.x.min(cb.x), ca.z.min(cb.z)),
                    GridCell::new(ca.x.max(cb.x) + 1, ca.z.max(cb.z) + 1),
                );
                Preview::Rect(
                    self.vertex_point((lo.x, lo.z)),
                    self.vertex_point((hi.x, hi.z)),
                )
            }
            _ => Preview::None,
        };
    }

    fn finish(&mut self, a: Vec3, b: Vec3) {
        let step = self.step();
        let floor = self.floor;
        match self.tool {
            Tool::WallLine => {
                let (va, vb) = self.line_end(a, b);
                if va == vb {
                    self.say("Линия нулевой длины".into());
                    return;
                }
                let plan = self.material_id().map_err(ToolError::Prefab).and_then(|m| {
                    tools::wall_path(
                        self.session.world(),
                        self.ids.as_mut(),
                        floor,
                        &[va, vb],
                        &m,
                        self.thickness,
                    )
                });
                self.run("Стены", plan);
            }
            Tool::Room => {
                let (ca, cb) = (snap_cell(a, step), snap_cell(b, step));
                let plan = self.material_id().map_err(ToolError::Prefab).and_then(|m| {
                    tools::room_rect(
                        self.session.world(),
                        self.ids.as_mut(),
                        floor,
                        ca,
                        cb,
                        &m,
                        self.thickness,
                        self.room_floor,
                        self.room_ceiling,
                    )
                });
                self.run("Комната", plan);
            }
            Tool::Door => self.place_door(b),
            Tool::Model => self.place_model(b),
            Tool::Stair => {
                let to = FloorId(floor.0 + 1);
                let start = snap_cell(b, step);
                let plan = self.material_id().map_err(ToolError::Prefab).and_then(|m| {
                    tools::stair_between(
                        self.session.world(),
                        self.ids.as_mut(),
                        floor,
                        to,
                        start,
                        self.stair_dir,
                        self.stair_len,
                        self.stair_ramp,
                        &m,
                        true,
                    )
                });
                self.run(
                    if self.stair_ramp {
                        "Пандус"
                    } else {
                        "Лестница"
                    },
                    plan,
                );
            }
            Tool::Select => {
                let moved = a.distance(b) > step * 0.5;
                if moved {
                    let (ca, cb) = (snap_cell(a, step), snap_cell(b, step));
                    let found = tools::select_rect(self.session.world(), floor, ca, cb);
                    self.apply_selection(found);
                } else if let Some(id) = self.element_under(b) {
                    self.apply_selection(BTreeSet::from([id]));
                } else if !self.additive_select {
                    self.selection.clear();
                }
            }
            Tool::Delete => {
                let target: BTreeSet<ObjectId> = if self.selection.is_empty() {
                    self.element_under(b).into_iter().collect()
                } else {
                    self.selection.clone()
                };
                let plan = tools::delete(self.session.world(), &target);
                self.run("Удаление", plan);
                self.selection.clear();
            }
        }
    }

    fn apply_selection(&mut self, found: BTreeSet<ObjectId>) {
        if self.additive_select {
            for id in found {
                if !self.selection.remove(&id) {
                    self.selection.insert(id);
                }
            }
        } else {
            self.selection = found;
        }
        self.say(format!("Выбрано: {}", self.selection.len()));
    }

    /// Элемент под точкой: ближайшее ребро (стена/проём), иначе перекрытие или марш в ячейке.
    pub fn element_under(&self, p: Vec3) -> Option<ObjectId> {
        let step = self.step();
        let w = self.world();
        let cell = snap_cell(p, step);
        // Расстояние до ближайшего ребра в долях шага: у самого ребра выбираем его.
        let edge = nearest_edge(p, step);
        let (u, v) = (p.x / step - cell.x as f32, p.z / step - cell.z as f32);
        let edge_dist = u.min(1.0 - u).min(v.min(1.0 - v));
        if edge_dist < 0.3
            && let Some(e) = w.edge_element(self.floor, edge)
        {
            return Some(e.id);
        }
        [
            SlotKey::Run(cell),
            SlotKey::FloorSlab(cell),
            SlotKey::CeilingSlab(cell),
        ]
        .into_iter()
        .find_map(|k| w.element_at(self.floor, k))
        .map(|e| e.id)
        .or_else(|| w.edge_element(self.floor, edge).map(|e| e.id))
    }

    fn place_model(&mut self, p: Vec3) {
        let Ok(model) = PrototypeId::new(self.model.trim()) else {
            self.say("Неверный идентификатор модели".into());
            return;
        };
        let pos = [p.x, self.floor_base_y(), p.z];
        let placement =
            astrocore_world::FreePlacement::from_position(pos, self.step(), 0.0, self.model_half);
        let element = StructureElement {
            id: self.ids.next_id(),
            kind: StructureKind::Freeform,
            placement: Placement::Free(placement),
            floor: self.floor,
            material: model,
            thickness: 0.1,
        };
        let id = element.id;
        self.run("Модель", Ok(vec![WorldCommand::PlaceElement(element)]));
        if self.session.exists(id) {
            self.selection = BTreeSet::from([id]);
        }
    }

    fn place_door(&mut self, p: Vec3) {
        let edge = nearest_edge(p, self.step());
        let floor = self.floor;
        let mut cmds = Vec::new();
        match self.session.world().edge_element(floor, edge) {
            Some(e) if matches!(e.kind, StructureKind::Wall) => {
                cmds.push(WorldCommand::RemoveElement { id: e.id })
            }
            Some(_) => {
                self.say("На этом ребре уже есть проём".into());
                return;
            }
            None => {}
        }
        let Ok(m) = self.material_id() else {
            self.say("Неверный идентификатор материала".into());
            return;
        };
        cmds.push(WorldCommand::PlaceElement(StructureElement {
            id: self.ids.next_id(),
            kind: StructureKind::Opening(OpeningKind::Door),
            placement: Placement::Edge(edge),
            floor,
            material: m,
            thickness: self.thickness.min(0.2),
        }));
        let r = self
            .session
            .execute("Дверь", cmds)
            .map_err(|e| e.to_string());
        self.report("Дверь", r);
    }

    // ---- кнопки ---------------------------------------------------------------

    pub fn undo(&mut self) {
        let r = self.session.undo().map(|_| ()).map_err(|e| e.to_string());
        self.report("отмена", r);
    }

    pub fn redo(&mut self) {
        let r = self.session.redo().map(|_| ()).map_err(|e| e.to_string());
        self.report("повтор", r);
    }

    pub fn add_floor(&mut self) {
        let n = self.world().floors().count();
        let (id, cmd) = tools::add_floor_above(self.world(), &format!("Этаж {n}"), 3.0);
        self.run("Новый этаж", Ok(vec![cmd]));
        if self.session.world().floor(id).is_some() {
            self.floor = id;
        }
    }

    pub fn auto_slabs(&mut self, ceiling: bool) {
        let kind = if ceiling {
            StructureKind::Ceiling
        } else {
            StructureKind::Floor
        };
        let plan = self.material_id().map_err(ToolError::Prefab).and_then(|m| {
            tools::auto_slabs(
                self.session.world(),
                self.ids.as_mut(),
                self.floor,
                kind,
                &m,
                self.thickness,
            )
        });
        self.run(
            if ceiling {
                "Потолок по контуру"
            } else {
                "Пол по контуру"
            },
            plan,
        );
    }

    pub fn replace_selected_material(&mut self) {
        let plan = self
            .material_id()
            .map_err(ToolError::Prefab)
            .and_then(|m| tools::replace_material(self.session.world(), &self.selection, &m));
        self.run("Замена материала", plan);
    }

    pub fn validate(&mut self) {
        self.findings = validate_map(self.world());
        self.say(if has_errors(&self.findings) {
            "Валидатор: есть ошибки".into()
        } else {
            format!(
                "Валидатор: ошибок нет, предупреждений {}",
                self.findings
                    .iter()
                    .filter(|f| f.severity == Severity::Warning)
                    .count()
            )
        });
    }

    pub fn set_view(&mut self, view: FloorView) {
        self.view = view;
        self.stamp += 1;
    }

    /// Фильтр видимости для рендера по текущему режиму просмотра.
    pub fn render_filter(&self) -> Option<ElementFilter> {
        use astrocore_editor::view::{Visibility, visibility};
        let view = self.view;
        if view == FloorView::All {
            return None;
        }
        let world = Arc::new(self.world().clone());
        Some(Arc::new(move |e: &StructureElement| {
            visibility(&world, view, e) != Visibility::Hidden
        }))
    }

    // ---- проект ---------------------------------------------------------------

    pub fn save(&mut self, path: &Path) {
        let mut project = Project::new("studio", self.world().clone());
        project.prefabs = self.prefabs.clone();
        project.instances = self.instances.clone();
        match project.save(path) {
            Ok(()) => {
                self.baseline = Some(self.world().clone());
                self.project_path = Some(path.to_path_buf());
                self.say(format!("Сохранено: {}", path.display()));
            }
            Err(e) => self.say(format!("Не сохранилось: {e}")),
        }
    }

    pub fn load(&mut self, path: &Path) {
        match Project::load(path) {
            Ok(p) => {
                let ids = Box::new(RandomIds);
                let console = std::mem::take(&mut self.console);
                *self = Self::with_world(p.world, ids);
                self.console = console;
                self.baseline = Some(self.world().clone());
                self.prefabs = p.prefabs;
                self.instances = p.instances;
                self.project_path = Some(path.to_path_buf());
                self.say(format!("Открыто: {}", path.display()));
            }
            Err(e) => self.say(format!("Не открылось: {e}")),
        }
    }

    // ---- инспектор ------------------------------------------------------------

    /// Единственный выбранный элемент.
    pub fn selected_element(&self) -> Option<&StructureElement> {
        let mut it = self.selection.iter();
        let id = it.next()?;
        if it.next().is_some() {
            return None;
        }
        self.world().element(*id)
    }

    /// Поля выбранного элемента для панели свойств.
    pub fn inspect(&self) -> Option<Inspector> {
        let e = self.selected_element()?;
        let step = self.step();
        let free = match e.placement {
            Placement::Free(f) => Some(FreeFields {
                position: f.position(step),
                yaw_deg: f.yaw_cdeg as f32 / 100.0,
                half_m: f.half_mm.map(|h| h as f32 / 1000.0),
            }),
            _ => None,
        };
        Some(Inspector {
            id: e.id,
            kind: format!("{:?}", e.kind),
            material: e.material.to_string(),
            thickness: e.thickness,
            free,
        })
    }

    /// Применить правки панели свойств одной операцией (один шаг отмены).
    pub fn apply_inspector(&mut self, draft: &Inspector) {
        let Some(old) = self.world().element(draft.id).cloned() else {
            self.say("Объект не найден".into());
            return;
        };
        let Ok(material) = PrototypeId::new(draft.material.trim()) else {
            self.say("Неверный идентификатор материала".into());
            return;
        };
        if !(draft.thickness.is_finite() && draft.thickness > 0.0) {
            self.say("Толщина должна быть положительной".into());
            return;
        }
        let mut new = old.clone();
        new.material = material;
        new.thickness = draft.thickness;
        if let (Placement::Free(_), Some(f)) = (old.placement, &draft.free) {
            new.placement = Placement::Free(astrocore_world::FreePlacement::from_position(
                f.position,
                self.step(),
                f.yaw_deg,
                f.half_m,
            ));
        }
        if new == old {
            return;
        }
        self.replace_element("Свойства", old, new);
    }

    fn replace_element(&mut self, label: &str, old: StructureElement, new: StructureElement) {
        let cmds = vec![
            WorldCommand::RemoveElement { id: old.id },
            WorldCommand::PlaceElement(new),
        ];
        self.run(label, Ok(cmds));
    }

    // ---- гизмо ----------------------------------------------------------------

    /// Положение гизмо: ноги выбранной свободной модели.
    pub fn gizmo_origin(&self) -> Option<Vec3> {
        match self.selected_element()?.placement {
            Placement::Free(f) => {
                let p = f.position(self.step());
                Some(Vec3::new(p[0], p[1], p[2]))
            }
            _ => None,
        }
    }

    /// Нажатие на гизмо: возвращает true, если захвачена ось (тогда курсор не рисует и не выбирает).
    pub fn gizmo_press(&mut self, ray: Ray) -> bool {
        let Some(origin) = self.gizmo_origin() else {
            return false;
        };
        let Some(axis) = pick_axis(origin, ray, GIZMO_LENGTH, 0.18) else {
            return false;
        };
        self.gizmo = AxisDrag::begin(axis, origin, ray);
        self.gizmo.is_some()
    }

    pub fn gizmo_axis(&self) -> Option<Axis> {
        self.gizmo.map(|d| d.axis)
    }

    /// Перетаскивание: модель идёт за курсором вдоль оси с привязкой к `snap_step` (0 — без неё).
    pub fn gizmo_drag(&mut self, ray: Ray, snap_step: f32) {
        let (Some(drag), Some(old)) = (self.gizmo, self.selected_element().cloned()) else {
            return;
        };
        let Placement::Free(f) = old.placement else {
            return;
        };
        let moved = drag.origin + drag.delta(ray);
        let target = Vec3::new(
            snap(moved.x, snap_step),
            snap(moved.y, snap_step),
            snap(moved.z, snap_step),
        );
        let half = f.half_mm.map(|h| h as f32 / 1000.0);
        let mut new = old.clone();
        new.placement = Placement::Free(astrocore_world::FreePlacement::from_position(
            [target.x, target.y, target.z],
            self.step(),
            f.yaw_cdeg as f32 / 100.0,
            half,
        ));
        // Положение хранится как «цель»: в мир и историю оно попадает одной операцией при отпускании.
        self.gizmo_target = Some(new);
    }

    /// Где окажется модель, если отпустить сейчас (для «призрака» при перетаскивании).
    pub fn gizmo_ghost(&self) -> Option<Vec3> {
        let t = self.gizmo_target.as_ref()?;
        match t.placement {
            Placement::Free(f) => {
                let p = f.position(self.step());
                Some(Vec3::new(p[0], p[1], p[2]))
            }
            _ => None,
        }
    }

    pub fn gizmo_release(&mut self) {
        if self.gizmo.take().is_none() {
            return;
        }
        let target = self.gizmo_target.take();
        let old = self.selected_element().cloned();
        if let (Some(new), Some(old)) = (target, old)
            && new != old
        {
            self.replace_element("Перемещение", old, new);
        }
    }

    /// Повернуть выбранную модель так, чтобы «вперёд» смотрело на курсор.
    pub fn rotate_selected_toward(&mut self, ray: Ray, snap_deg: f32) {
        let (Some(origin), Some(old)) = (self.gizmo_origin(), self.selected_element().cloned())
        else {
            return;
        };
        let (Placement::Free(f), Some(yaw)) = (old.placement, yaw_toward(origin, ray)) else {
            return;
        };
        let mut new = old.clone();
        new.placement = Placement::Free(astrocore_world::FreePlacement {
            yaw_cdeg: (snap(yaw, snap_deg) * 100.0).round() as i32,
            ..f
        });
        if new != old {
            self.replace_element("Поворот", old, new);
        }
    }

    // ---- префабы --------------------------------------------------------------

    pub fn prefab_ids(&self) -> Vec<String> {
        self.prefabs.ids().map(|i| i.to_string()).collect()
    }

    /// Разместить экземпляр префаба: смещение в ячейках, число четвертей оборота, текущий этаж.
    pub fn place_prefab(&mut self, id: &str, dx: i32, dz: i32, quarter_turns: u8) {
        let Ok(pid) = PrototypeId::new(id) else {
            self.say("Неверный идентификатор префаба".into());
            return;
        };
        let mut instance = PrefabInstance::new(astrocore_core::PrefabInstanceId::new_random(), pid);
        instance.base_floor = self.floor.0;
        instance.transform = GridTransform {
            quarter_turns: quarter_turns % 4,
            ..GridTransform::translate(dx, dz)
        };
        match tools::place_prefab(
            &self.prefabs,
            &instance,
            &std::collections::BTreeSet::new(),
            self.session.world(),
        ) {
            Ok(plan) => {
                let spawned = plan.spawned.clone();
                let conflicts = plan.conflicts.len();
                self.run("Префаб", Ok(plan.commands));
                if !spawned.is_empty() && spawned.iter().all(|i| self.session.exists(*i)) {
                    self.instances.push(InstanceRecord { instance, spawned });
                }
                if conflicts > 0 {
                    self.say(format!("Префаб: конфликтов {conflicts}"));
                }
            }
            Err(e) => self.say(format!("Префаб: {e}")),
        }
    }

    // ---- различия -------------------------------------------------------------

    /// Что изменилось с момента открытия или сохранения проекта.
    pub fn diff_since_save(&self) -> Option<WorldDiff> {
        self.baseline
            .as_ref()
            .map(|base| diff_worlds(base, self.world()))
    }

    pub fn new_project(&mut self) {
        *self = Self::new();
    }
}

impl Default for StudioState {
    fn default() -> Self {
        Self::new()
    }
}
