use astrocore_editor::tools::SequentialIds;
use astrocore_editor::view::FloorView;
use astrocore_math::glam::Vec3;
use astrocore_math::{Facing, GridCell, GridEdge};
use astrocore_world::{FloorId, OpeningKind, Placement, SlotKey, StructureKind};

use super::*;

fn studio() -> StudioState {
    let mut s = StudioState::with_world(
        {
            let mut w =
                astrocore_world::World::new("t", astrocore_world::WorldConfig::default()).unwrap();
            for f in 0..2 {
                w.apply(astrocore_world::WorldCommand::DefineFloor(
                    astrocore_world::FloorDef {
                        id: FloorId(f),
                        name: format!("f{f}"),
                        base_y: f as f32 * 4.0,
                        height: 4.0,
                    },
                ))
                .unwrap();
            }
            w
        },
        Box::new(SequentialIds(0)),
    );
    s.material = "t:stone".into();
    s
}

fn p(x: f32, z: f32) -> Vec3 {
    Vec3::new(x, 0.0, z)
}

fn drag(s: &mut StudioState, from: Vec3, to: Vec3) {
    s.pointer(Pointer::Press(from));
    s.pointer(Pointer::Drag(to));
    s.pointer(Pointer::Release(to));
}

fn click(s: &mut StudioState, at: Vec3) {
    s.pointer(Pointer::Press(at));
    s.pointer(Pointer::Release(at));
}

#[test]
fn wall_line_snaps_to_vertices_and_axes_and_is_undoable() {
    let mut s = studio();
    s.tool = Tool::WallLine;
    s.pointer(Pointer::Press(p(0.2, 0.1)));
    s.pointer(Pointer::Drag(p(4.1, 0.9)));
    assert_eq!(
        s.preview,
        Preview::Line(p(0.0, 0.0), p(4.0, 0.0)),
        "линия прижата к вершинам и оси X"
    );
    s.pointer(Pointer::Release(p(4.1, 0.9)));
    assert_eq!(s.world().element_count(), 4);
    assert_eq!(s.preview, Preview::None);
    assert!(s.status.starts_with("Выполнено"));
    s.undo();
    assert_eq!(s.world().element_count(), 0);
    s.redo();
    assert_eq!(s.world().element_count(), 4);
    // Вертикальная линия.
    drag(&mut s, p(10.1, 0.0), p(10.4, 3.2));
    assert_eq!(s.world().element_count(), 7);
    // Нулевая длина и повтор по тем же рёбрам ничего не добавляют.
    click(&mut s, p(1.0, 1.0));
    assert!(s.status.contains("нулевой"));
    drag(&mut s, p(0.0, 0.0), p(4.0, 0.0));
    assert_eq!(s.world().element_count(), 7);
}

#[test]
fn room_tool_builds_walls_floor_and_optional_ceiling() {
    let mut s = studio();
    s.tool = Tool::Room;
    s.room_ceiling = true;
    drag(&mut s, p(1.2, 1.2), p(3.8, 2.6));
    // Клетки x 1..=3, z 1..=2: 3×2 = 6 клеток, периметр 10 рёбер.
    assert_eq!(s.world().element_count(), 10 + 6 + 6);
    s.undo();
    assert_eq!(s.world().element_count(), 0);
    s.room_floor = false;
    s.room_ceiling = false;
    s.redo();
    s.undo();
    drag(&mut s, p(1.2, 1.2), p(3.8, 2.6));
    assert_eq!(s.world().element_count(), 10, "только стены");
}

#[test]
fn auto_slabs_fill_a_closed_contour() {
    let mut s = studio();
    s.tool = Tool::Room;
    s.room_floor = false;
    drag(&mut s, p(0.5, 0.5), p(2.5, 2.5));
    assert_eq!(s.world().element_count(), 12);
    s.auto_slabs(false);
    s.auto_slabs(true);
    assert_eq!(s.world().element_count(), 12 + 9 + 9);
    s.auto_slabs(false);
    assert!(s.status.starts_with("Не получилось"), "{}", s.status);
}

#[test]
fn door_replaces_a_wall_and_refuses_double_openings() {
    let mut s = studio();
    s.tool = Tool::Room;
    drag(&mut s, p(0.5, 0.5), p(2.5, 2.5));
    s.tool = Tool::Door;
    click(&mut s, p(1.5, 0.05)); // северная стена комнаты, клетка x=1
    let door = s
        .world()
        .edge_element(FloorId(0), GridEdge::along_x(1, 0))
        .unwrap();
    assert!(matches!(
        door.kind,
        StructureKind::Opening(OpeningKind::Door)
    ));
    let count = s.world().element_count();
    click(&mut s, p(1.5, 0.05));
    assert!(s.status.contains("уже есть проём"));
    assert_eq!(s.world().element_count(), count);
    s.undo();
    assert!(
        matches!(
            s.world()
                .edge_element(FloorId(0), GridEdge::along_x(1, 0))
                .unwrap()
                .kind,
            StructureKind::Wall
        ),
        "undo вернул стену"
    );
    // Дверь в свободном месте тоже ставится.
    click(&mut s, p(20.5, 20.05));
    assert!(
        s.world()
            .edge_element(FloorId(0), GridEdge::along_x(20, 20))
            .is_some()
    );
}

#[test]
fn stair_tool_pierces_and_needs_an_upper_floor() {
    let mut s = studio();
    // Перекрытия на втором этаже над будущей лестницей.
    s.floor = FloorId(1);
    s.tool = Tool::Room;
    s.room_floor = true;
    drag(&mut s, p(0.5, 0.5), p(8.5, 1.5));
    let slabs_before = s.world().element_count();
    s.floor = FloorId(0);
    s.tool = Tool::Stair;
    s.stair_dir = Facing::East;
    s.stair_len = 4;
    click(&mut s, p(2.5, 0.5));
    assert_eq!(s.world().vertical_links().len(), 1);
    assert_eq!(
        s.world().element_count(),
        slabs_before + 1 - 4,
        "4 клетки перекрытия убраны, лестница добавлена"
    );
    assert!(
        s.world()
            .element_at(FloorId(1), SlotKey::FloorSlab(GridCell::new(3, 0)))
            .is_none()
    );
    s.undo();
    assert_eq!(s.world().element_count(), slabs_before);
    // С верхнего этажа вести некуда.
    s.floor = FloorId(1);
    click(&mut s, p(2.5, 5.5));
    assert!(s.status.starts_with("Не получилось"), "{}", s.status);
}

#[test]
fn select_click_rectangle_additive_and_delete() {
    let mut s = studio();
    s.tool = Tool::Room;
    drag(&mut s, p(0.5, 0.5), p(3.5, 3.5));
    let total = s.world().element_count();
    s.tool = Tool::Select;
    click(&mut s, p(1.5, 0.05)); // у стены
    assert_eq!(s.selection.len(), 1);
    let wall = *s.selection.iter().next().unwrap();
    assert!(matches!(
        s.world().element(wall).unwrap().kind,
        StructureKind::Wall
    ));
    click(&mut s, p(2.5, 2.5)); // в середине клетки — перекрытие
    assert!(matches!(
        s.world()
            .element(*s.selection.iter().next().unwrap())
            .unwrap()
            .kind,
        StructureKind::Floor
    ));
    // Прямоугольное выделение берёт всё внутри.
    drag(&mut s, p(0.1, 0.1), p(3.9, 3.9));
    assert_eq!(s.selection.len(), total);
    // Аддитивный клик снимает выбранное.
    s.additive_select = true;
    click(&mut s, p(2.5, 2.5));
    assert_eq!(s.selection.len(), total - 1);
    s.additive_select = false;
    click(&mut s, p(50.0, 50.0)); // пустое место сбрасывает выбор
    assert!(s.selection.is_empty());

    // Удаление под курсором без выбора.
    s.tool = Tool::Delete;
    click(&mut s, p(2.5, 2.5));
    assert_eq!(s.world().element_count(), total - 1);
    // Удаление выбранного.
    s.tool = Tool::Select;
    drag(&mut s, p(0.1, 0.1), p(3.9, 3.9));
    s.tool = Tool::Delete;
    click(&mut s, p(0.0, 0.0));
    assert_eq!(s.world().element_count(), 0);
    assert!(s.selection.is_empty());
    s.undo();
    assert_eq!(s.world().element_count(), total - 1);
}

#[test]
fn material_changes_apply_to_the_selection_and_bad_ids_are_reported() {
    let mut s = studio();
    s.tool = Tool::WallLine;
    drag(&mut s, p(0.0, 0.0), p(3.0, 0.0));
    s.tool = Tool::Select;
    drag(&mut s, p(0.0, -0.2), p(3.0, 0.2));
    s.material = "t:wood".into();
    s.replace_selected_material();
    assert!(
        s.world()
            .elements()
            .all(|e| e.material.as_str() == "t:wood")
    );
    s.material = "Плохой ID".into();
    s.replace_selected_material();
    assert!(s.status.starts_with("Не получилось"));
    s.tool = Tool::WallLine;
    drag(&mut s, p(10.0, 0.0), p(12.0, 0.0));
    assert!(s.status.starts_with("Не получилось"), "{}", s.status);
    assert_eq!(s.world().element_count(), 3);
}

#[test]
fn floors_views_and_render_filter() {
    let mut s = studio();
    s.tool = Tool::Room;
    s.room_ceiling = true;
    drag(&mut s, p(0.5, 0.5), p(1.5, 1.5));
    s.add_floor();
    assert_eq!(s.floor, FloorId(2));
    assert!((s.floor_base_y() - 8.0).abs() < 1e-5);
    s.floor = FloorId(1);
    drag(&mut s, p(0.5, 0.5), p(1.5, 1.5));
    assert!(s.render_filter().is_none(), "режим «все» — без фильтра");
    let stamp = s.stamp;
    s.set_view(FloorView::Slice { top: FloorId(0) });
    assert!(s.stamp > stamp);
    let filter = s.render_filter().unwrap();
    let kept = |floor: i32, kind: fn(&StructureKind) -> bool| {
        s.world()
            .elements()
            .filter(|e| e.floor == FloorId(floor) && kind(&e.kind))
            .all(|e| filter(e))
    };
    assert!(kept(0, |k| matches!(k, StructureKind::Wall)));
    assert!(
        !kept(0, |k| matches!(k, StructureKind::Ceiling)),
        "потолок среза скрыт"
    );
    assert!(
        !kept(1, |k| matches!(k, StructureKind::Wall)),
        "верхний этаж скрыт"
    );
}

#[test]
fn validation_and_project_roundtrip() {
    let mut s = studio();
    s.tool = Tool::Room;
    drag(&mut s, p(0.5, 0.5), p(2.5, 2.5));
    s.validate();
    assert!(s.status.contains("ошибок нет"));
    assert!(
        !s.findings.is_empty(),
        "замкнутая комната без прохода — предупреждение"
    );

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("map.acproj");
    let hash = s.world().content_hash();
    s.save(&path);
    assert_eq!(s.project_path.as_deref(), Some(path.as_path()));
    s.new_project();
    assert_eq!(s.world().element_count(), 0);
    s.load(&path);
    assert_eq!(s.world().content_hash(), hash);
    s.load(&dir.path().join("нет.acproj"));
    assert!(s.status.starts_with("Не открылось"));
    s.save(&dir.path().join("нет-каталога/x/map.acproj"));
    assert!(s.status.starts_with("Не сохранилось"));
}

#[test]
fn cancel_drops_the_drag() {
    let mut s = studio();
    s.tool = Tool::WallLine;
    s.pointer(Pointer::Press(p(0.0, 0.0)));
    s.pointer(Pointer::Drag(p(5.0, 0.0)));
    assert!(matches!(s.preview, Preview::Line(..)));
    s.cancel();
    s.pointer(Pointer::Release(p(5.0, 0.0)));
    assert_eq!(s.world().element_count(), 0);
    let _ = Placement::Cell(GridCell::new(0, 0));
}
