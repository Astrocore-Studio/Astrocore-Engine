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

mod workspace_panels {
    use crate::state::CONSOLE_LIMIT;
    use astrocore_core::{ObjectId, PrototypeId};
    use astrocore_editor::gizmo::{Axis, Ray};
    use astrocore_prefabs::{
        ElementTemplate, LocalKind, LocalPlacement, NodeContent, PrefabDef, PrefabNode,
    };

    use super::*;

    fn model_at(s: &mut StudioState, x: f32, z: f32) -> ObjectId {
        s.tool = Tool::Model;
        s.model = "t:statue".into();
        click(s, p(x, z));
        *s.selection
            .iter()
            .next()
            .expect("модель выбрана после размещения")
    }

    fn look_at(from: Vec3, to: Vec3) -> Ray {
        Ray {
            origin: from,
            dir: (to - from).normalize(),
        }
    }

    #[test]
    fn model_tool_places_a_freeform_element_and_selects_it() {
        let mut s = studio();
        let id = model_at(&mut s, 5.4, 6.2);
        let e = s.world().element(id).unwrap();
        assert_eq!(e.kind, StructureKind::Freeform);
        assert_eq!(e.material.as_str(), "t:statue");
        let o = s.gizmo_origin().unwrap();
        assert!(
            (o.x - 5.4).abs() < 0.01 && (o.z - 6.2).abs() < 0.01 && o.y == 0.0,
            "{o}"
        );
        s.undo();
        assert!(s.world().element(id).is_none());
        s.model = "плохо!".into();
        click(&mut s, p(1.0, 1.0));
        assert!(s.status.contains("Неверный идентификатор модели"));
    }

    #[test]
    fn inspector_edits_are_validated_and_one_undo_step() {
        let mut s = studio();
        let id = model_at(&mut s, 3.0, 3.0);
        let mut d = s.inspect().unwrap();
        assert_eq!(d.id, id);
        assert_eq!(d.kind, "Freeform");
        let f = d.free.as_mut().unwrap();
        f.position = [7.5, 0.0, 2.0];
        f.yaw_deg = 90.0;
        d.material = "t:marble".into();
        s.apply_inspector(&d);
        let after = s.inspect().unwrap();
        assert_eq!(after.material, "t:marble");
        let free = after.free.unwrap();
        assert!((free.position[0] - 7.5).abs() < 0.01 && (free.yaw_deg - 90.0).abs() < 0.01);
        s.undo();
        let undone = s.inspect().unwrap();
        assert_eq!(
            undone.material, "t:statue",
            "все правки панели откатываются одним шагом"
        );
        // Ошибочный ввод не меняет мир.
        let mut bad = undone.clone();
        bad.thickness = -1.0;
        s.apply_inspector(&bad);
        assert!(s.status.contains("Толщина"));
        bad.thickness = 0.1;
        bad.material = "!".into();
        s.apply_inspector(&bad);
        assert!(s.status.contains("материала"));
        assert_eq!(s.inspect().unwrap(), undone);
    }

    #[test]
    fn gizmo_drags_along_an_axis_and_commits_once() {
        let mut s = studio();
        let id = model_at(&mut s, 4.0, 4.0);
        let cam = Vec3::new(9.0, 7.0, 9.0);
        let o = s.gizmo_origin().unwrap();
        assert!(
            !s.gizmo_press(look_at(cam, Vec3::new(30.0, 0.0, 30.0))),
            "мимо оси"
        );
        assert!(s.gizmo_press(look_at(cam, o + Vec3::X)), "ось X");
        assert_eq!(s.gizmo_axis(), Some(Axis::X));
        let revision = s.world().revision();
        s.gizmo_drag(look_at(cam, o + Vec3::X * 3.0), 0.0);
        s.gizmo_drag(look_at(cam, o + Vec3::X * 4.0), 0.0);
        assert_eq!(
            s.world().revision(),
            revision,
            "мир не меняется до отпускания"
        );
        let ghost = s.gizmo_ghost().unwrap();
        assert!(
            (ghost.x - (o.x + 3.0)).abs() < 0.05 && (ghost.z - o.z).abs() < 0.01,
            "{ghost}"
        );
        s.gizmo_release();
        let moved = s.gizmo_origin().unwrap();
        assert!((moved.x - (o.x + 3.0)).abs() < 0.05 && (moved.z - o.z).abs() < 0.01);
        s.undo();
        assert!(
            (s.gizmo_origin().unwrap() - o).length() < 0.01,
            "одно перемещение — один шаг отмены"
        );
        assert!(s.world().element(id).is_some());
        // Шаг привязки.
        assert!(s.gizmo_press(look_at(cam, o + Vec3::X)));
        s.gizmo_drag(look_at(cam, o + Vec3::X * 2.2), 1.0);
        s.gizmo_release();
        let snapped = s.gizmo_origin().unwrap();
        assert!((snapped.x - snapped.x.round()).abs() < 0.01, "{snapped}");
    }

    #[test]
    fn rotation_handle_points_the_model_at_the_cursor() {
        let mut s = studio();
        model_at(&mut s, 5.0, 5.0);
        let o = s.gizmo_origin().unwrap();
        let down = |p: Vec3| Ray {
            origin: Vec3::new(p.x, 10.0, p.z),
            dir: Vec3::NEG_Y,
        };
        s.rotate_selected_toward(down(o + Vec3::new(-4.0, 0.0, 0.1)), 15.0);
        let yaw = s.inspect().unwrap().free.unwrap().yaw_deg;
        assert!((yaw - 90.0).abs() < 0.01, "{yaw}");
    }

    #[test]
    fn prefab_browser_places_instances_and_projects_keep_them() {
        let mut s = studio();
        let mut def = PrefabDef::new(PrototypeId::new("t:booth").unwrap());
        for (i, e) in [GridEdge::along_x(0, 0), GridEdge::along_x(0, 2)]
            .into_iter()
            .enumerate()
        {
            def.nodes.push(PrefabNode {
                id: astrocore_core::PrefabNodeId::from_u128(i as u128 + 1),
                name: format!("w{i}"),
                content: NodeContent::Element(ElementTemplate {
                    kind: LocalKind::Wall,
                    placement: LocalPlacement::Edge(e),
                    floor_offset: 0,
                    material: PrototypeId::new("t:stone").unwrap(),
                    thickness: 0.2,
                }),
            });
        }
        s.prefabs.insert(def).unwrap();
        assert_eq!(s.prefab_ids(), vec!["t:booth".to_string()]);
        s.place_prefab("t:booth", 10, 10, 0);
        assert_eq!(s.world().element_count(), 2);
        assert_eq!(s.instances.len(), 1);
        s.place_prefab("t:nope", 0, 0, 0);
        assert!(s.status.contains("Префаб"));
        // Тот же префаб на занятом месте: конфликт, второго экземпляра нет.
        let before = s.instances.len();
        s.place_prefab("t:booth", 10, 10, 0);
        assert_eq!(s.instances.len(), before);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.acproj");
        s.save(&path);
        let mut reopened = StudioState::new();
        reopened.load(&path);
        assert_eq!(reopened.prefab_ids(), vec!["t:booth".to_string()]);
        assert_eq!(reopened.instances.len(), 1);
        assert_eq!(reopened.world().element_count(), 2);
    }

    #[test]
    fn diff_since_save_and_console_track_work() {
        let mut s = studio();
        assert!(s.diff_since_save().is_none(), "до сохранения основы нет");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.acproj");
        s.save(&path);
        assert!(s.diff_since_save().unwrap().is_empty());
        let id = model_at(&mut s, 2.0, 2.0);
        let d = s.diff_since_save().unwrap();
        assert_eq!(d.added, vec![id]);
        assert!(d.summary().starts_with("+1"));
        assert!(s.console.iter().any(|l| l.starts_with("Сохранено")));
        assert!(s.console.iter().any(|l| l.contains("Модель")));
        for i in 0..CONSOLE_LIMIT + 20 {
            s.say(format!("строка {i}"));
        }
        assert_eq!(s.console.len(), CONSOLE_LIMIT);
        assert_eq!(
            s.console.last().unwrap(),
            &format!("строка {}", CONSOLE_LIMIT + 19)
        );
    }
}

#[test]
fn overlays_expose_collision_rooms_and_wireframe_of_the_current_floor() {
    let mut s = studio();
    s.tool = Tool::Room;
    drag(&mut s, p(0.5, 0.5), p(3.5, 3.5));
    assert!(
        s.overlay_geometry().collision.is_empty(),
        "оверлеи выключены"
    );
    s.overlays = Overlays {
        collision: true,
        topology: true,
        wireframe: true,
    };
    let g = s.overlay_geometry().clone();
    assert!(!g.collision.is_empty());
    assert_eq!(g.rooms.len(), 16, "комната 4×4 ячейки: {}", g.rooms.len());
    assert!(
        g.rooms
            .iter()
            .all(|(id, _, step)| *id == g.rooms[0].0 && *step == 1.0)
    );
    assert!(g.wire.len().is_multiple_of(3) && !g.wire.is_empty());
    // Кэш: без изменений мира геометрия та же; после правки — обновляется.
    assert_eq!(s.overlay_geometry().clone(), g);
    s.tool = Tool::Delete;
    s.selection.clear();
    click(&mut s, p(0.0, 1.5));
    assert_ne!(s.overlay_geometry().clone(), g);
    // Другой этаж: на нём пока ничего нет.
    s.floor = FloorId(1);
    assert!(s.overlay_geometry().collision.is_empty());
}
