use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use astrocore_core::{ObjectId, PrototypeId};
use astrocore_editor::EditSession;
use astrocore_editor::project::Project;
use astrocore_editor::snap::{nearest_edge, snap_cell, snap_vertex};
use astrocore_editor::tools::{self, IdGen, RandomIds, ToolError};
use astrocore_editor::validate::{Finding, Severity, has_errors, validate_map};
use astrocore_editor::view::FloorView;
use astrocore_math::glam::Vec3;

/// Фильтр видимости элементов для рендера.
pub type ElementFilter = Arc<dyn Fn(&StructureElement) -> bool + Send + Sync>;
use astrocore_math::{Facing, GridCell};
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
    Delete,
}

impl Tool {
    pub const ALL: [Tool; 6] = [
        Tool::Select,
        Tool::WallLine,
        Tool::Room,
        Tool::Door,
        Tool::Stair,
        Tool::Delete,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Tool::Select => "Выбор",
            Tool::WallLine => "Стены (линия)",
            Tool::Room => "Комната",
            Tool::Door => "Дверь",
            Tool::Stair => "Лестница",
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
    /// Растёт при каждом изменении мира или режима просмотра — оболочка по нему обновляет рендер.
    pub stamp: u64,
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

    fn material_id(&self) -> Result<PrototypeId, String> {
        PrototypeId::new(self.material.trim()).map_err(|e| format!("материал: {e}"))
    }

    fn report(&mut self, label: &str, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.status = format!("Выполнено: {label}");
                self.stamp += 1;
                self.selection.retain(|id| self.session.exists(*id));
            }
            Err(e) => self.status = format!("Не получилось: {e}"),
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
                    self.status = "Линия нулевой длины".into();
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
        self.status = format!("Выбрано: {}", self.selection.len());
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

    fn place_door(&mut self, p: Vec3) {
        let edge = nearest_edge(p, self.step());
        let floor = self.floor;
        let mut cmds = Vec::new();
        match self.session.world().edge_element(floor, edge) {
            Some(e) if matches!(e.kind, StructureKind::Wall) => {
                cmds.push(WorldCommand::RemoveElement { id: e.id })
            }
            Some(_) => {
                self.status = "На этом ребре уже есть проём".into();
                return;
            }
            None => {}
        }
        let Ok(m) = self.material_id() else {
            self.status = "Неверный идентификатор материала".into();
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
        self.status = if has_errors(&self.findings) {
            "Валидатор: есть ошибки".into()
        } else {
            format!(
                "Валидатор: ошибок нет, предупреждений {}",
                self.findings
                    .iter()
                    .filter(|f| f.severity == Severity::Warning)
                    .count()
            )
        };
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
        let project = Project::new("studio", self.world().clone());
        match project.save(path) {
            Ok(()) => {
                self.project_path = Some(path.to_path_buf());
                self.status = format!("Сохранено: {}", path.display());
            }
            Err(e) => self.status = format!("Не сохранилось: {e}"),
        }
    }

    pub fn load(&mut self, path: &Path) {
        match Project::load(path) {
            Ok(p) => {
                let ids = Box::new(RandomIds);
                *self = Self::with_world(p.world, ids);
                self.project_path = Some(path.to_path_buf());
                self.status = format!("Открыто: {}", path.display());
            }
            Err(e) => self.status = format!("Не открылось: {e}"),
        }
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
