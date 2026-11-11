use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use astrocore_core::{ObjectId, PrototypeId};
use astrocore_world::{OpeningKind, Placement, StructureKind};
use bevy::prelude::*;

use crate::manifest::{DoorEntry, ManifestError, SheetEntry, load_png, safe_join};
use crate::{RenderedWorld, material_color};

/// Лист одного состояния: картинка и сетка кадров.
#[derive(Clone)]
pub struct StateSheet {
    pub image: Handle<Image>,
    pub cols: u32,
    pub rows: u32,
    pub delays: Vec<f32>,
}

impl StateSheet {
    pub fn frames(&self) -> usize {
        self.delays.len().max(1)
    }

    pub fn cycle_seconds(&self) -> f32 {
        self.delays.iter().sum()
    }
}

#[derive(Clone)]
pub struct DoorSet {
    pub closed: StateSheet,
    pub open: StateSheet,
    pub opening: StateSheet,
    pub closing: StateSheet,
}

/// Спрайты дверей по идентификатору материала проёма.
#[derive(Resource, Default, Clone)]
pub struct DoorSprites(pub HashMap<PrototypeId, DoorSet>);

/// Целевое состояние дверей мира (открыта ли): заполняет клиент по серверным сообщениям.
#[derive(Resource, Default, Clone)]
pub struct DoorStates(pub HashMap<ObjectId, bool>);

/// Фаза анимации двери.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Closed,
    Opening,
    Open,
    Closing,
}

/// Состояние анимации: чистая логика, не зависит от Bevy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DoorAnim {
    pub phase: Phase,
    /// Время в текущей фазе, секунды.
    pub t: f32,
}

impl DoorAnim {
    pub fn new(open: bool) -> Self {
        Self {
            phase: if open { Phase::Open } else { Phase::Closed },
            t: 0.0,
        }
    }

    /// Продвинуть анимацию на `dt` секунд при целевом состоянии `want_open`. `opening` и
    /// `closing` — длительности фаз. Смена цели посреди анимации разворачивает её с
    /// соответствующего места, а не с начала, чтобы створки не «прыгали».
    pub fn advance(&mut self, want_open: bool, dt: f32, opening: f32, closing: f32) {
        match (self.phase, want_open) {
            (Phase::Closed, true) => {
                self.phase = Phase::Opening;
                self.t = 0.0;
            }
            (Phase::Open, false) => {
                self.phase = Phase::Closing;
                self.t = 0.0;
            }
            (Phase::Opening, false) => {
                // Развернуть: доля пройденного пути остаётся той же.
                let progress = if opening > 0.0 {
                    (self.t / opening).min(1.0)
                } else {
                    1.0
                };
                self.phase = Phase::Closing;
                self.t = (1.0 - progress) * closing;
            }
            (Phase::Closing, true) => {
                let progress = if closing > 0.0 {
                    (self.t / closing).min(1.0)
                } else {
                    1.0
                };
                self.phase = Phase::Opening;
                self.t = (1.0 - progress) * opening;
            }
            _ => {}
        }
        self.t += dt;
        match self.phase {
            Phase::Opening if self.t >= opening => {
                self.phase = Phase::Open;
                self.t = 0.0;
            }
            Phase::Closing if self.t >= closing => {
                self.phase = Phase::Closed;
                self.t = 0.0;
            }
            _ => {}
        }
    }

    /// Закрыта ли дверь для прохода: пока створки не раскрылись, проход считается закрытым.
    pub fn is_passable(&self) -> bool {
        self.phase == Phase::Open
    }
}

/// Номер кадра по накопленному времени и задержкам (последний кадр держится).
pub fn frame_at(delays: &[f32], t: f32) -> usize {
    let mut acc = 0.0;
    for (i, d) in delays.iter().enumerate() {
        acc += d.max(0.0);
        if t < acc {
            return i;
        }
    }
    delays.len().saturating_sub(1)
}

fn load_sheet(
    e: &SheetEntry,
    base: &Path,
    images: &mut Assets<Image>,
) -> Result<StateSheet, ManifestError> {
    if e.cols == 0
        || e.rows == 0
        || e.cols > 64
        || e.rows > 64
        || e.delays.iter().any(|d| !d.is_finite() || *d < 0.0)
    {
        return Err(ManifestError::Parse(format!("лист {}", e.image)));
    }
    let img = load_png(&safe_join(base, &e.image)?, false)?;
    Ok(StateSheet {
        image: images.add(img),
        cols: e.cols,
        rows: e.rows,
        delays: e.delays.clone(),
    })
}

pub(crate) fn install_doors(
    doors: &BTreeMap<String, DoorEntry>,
    base: &Path,
    images: &mut Assets<Image>,
) -> Result<DoorSprites, ManifestError> {
    let mut out = DoorSprites::default();
    for (id, d) in doors {
        let pid = PrototypeId::new(id.as_str()).map_err(|e| ManifestError::Parse(e.to_string()))?;
        out.0.insert(
            pid,
            DoorSet {
                closed: load_sheet(&d.closed, base, images)?,
                open: load_sheet(&d.open, base, images)?,
                opening: load_sheet(&d.opening, base, images)?,
                closing: load_sheet(&d.closing, base, images)?,
            },
        );
    }
    Ok(out)
}

/// Панель двери в мире.
#[derive(Component)]
pub struct DoorPanel {
    pub id: ObjectId,
    pub set: PrototypeId,
    pub anim: DoorAnim,
    mesh_material: Handle<StandardMaterial>,
}

pub struct DoorSpritesPlugin;

impl Plugin for DoorSpritesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DoorSprites>()
            .init_resource::<DoorStates>()
            .add_systems(Update, (sync_door_panels, animate_door_panels).chain());
    }
}

/// Вертикальный прямоугольник шириной `w` и высотой `h` с началом в центре низа, лицом к +Z.
fn panel_mesh(w: f32, h: f32) -> Mesh {
    use bevy::asset::RenderAssetUsages;
    use bevy::mesh::{Indices, PrimitiveTopology};
    let hw = w * 0.5;
    let mut m = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    m.insert_attribute(
        Mesh::ATTRIBUTE_POSITION,
        vec![[-hw, 0.0, 0.0], [hw, 0.0, 0.0], [hw, h, 0.0], [-hw, h, 0.0]],
    );
    m.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, 1.0]; 4]);
    m.insert_attribute(
        Mesh::ATTRIBUTE_UV_0,
        vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
    );
    m.insert_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]));
    m
}

/// Держит панели в соответствии с дверями мира: у каждой двери с известным спрайтом — панель.
#[allow(clippy::too_many_arguments)]
fn sync_door_panels(
    mut commands: Commands,
    rendered: Option<Res<RenderedWorld>>,
    sprites: Res<DoorSprites>,
    states: Res<DoorStates>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    existing: Query<(Entity, &DoorPanel)>,
    mut seen: Local<Option<(u64, usize)>>,
) {
    let Some(rendered) = rendered else { return };
    let key = (rendered.world.revision().0, sprites.0.len());
    if *seen == Some(key) {
        return;
    }
    *seen = Some(key);
    let world = &rendered.world;
    let step = world.config().grid_step_m;
    let mut want: HashMap<ObjectId, (Transform, f32, f32, PrototypeId)> = HashMap::new();
    for e in world.elements() {
        let (StructureKind::Opening(OpeningKind::Door), Placement::Edge(edge)) =
            (e.kind, e.placement)
        else {
            continue;
        };
        if !sprites.0.contains_key(&e.material) {
            continue;
        }
        let Some(floor) = world.floor(e.floor) else {
            continue;
        };
        let (a, b) = edge.endpoints(step, floor.base_y);
        let along = Vec3::new(b.x - a.x, 0.0, b.z - a.z);
        let center = Vec3::new((a.x + b.x) * 0.5, floor.base_y, (a.z + b.z) * 0.5);
        // Панель лежит в плоскости проёма: её «вперёд» (+Z в локальных осях) — нормаль стены.
        let normal = Vec3::Y.cross(along.normalize_or_zero());
        let rot = Quat::from_rotation_arc(Vec3::Z, normal);
        want.insert(
            e.id,
            (
                Transform {
                    translation: center,
                    rotation: rot,
                    scale: Vec3::ONE,
                },
                along.length(),
                floor.height,
                e.material.clone(),
            ),
        );
    }
    let mut have: HashMap<ObjectId, Entity> = HashMap::new();
    for (en, p) in &existing {
        if want.contains_key(&p.id) && !have.contains_key(&p.id) {
            have.insert(p.id, en);
        } else {
            commands.entity(en).despawn();
        }
    }
    for (id, (tf, w, h, set)) in want {
        if have.contains_key(&id) {
            continue;
        }
        let open = states.0.get(&id).copied().unwrap_or(false);
        let initial = &sprites.0[&set];
        let sheet = if open { &initial.open } else { &initial.closed };
        let material = materials.add(StandardMaterial {
            base_color: material_color(&set),
            base_color_texture: Some(sheet.image.clone()),
            alpha_mode: AlphaMode::Mask(0.5),
            unlit: true,
            double_sided: true,
            cull_mode: None,
            ..default()
        });
        commands.spawn((
            Mesh3d(meshes.add(panel_mesh(w, h))),
            MeshMaterial3d(material.clone()),
            tf,
            DoorPanel {
                id,
                set,
                anim: DoorAnim::new(open),
                mesh_material: material,
            },
        ));
    }
}

fn set_frame(mat: &mut StandardMaterial, sheet: &StateSheet, frame: usize) {
    let (cols, rows) = (sheet.cols.max(1), sheet.rows.max(1));
    let idx = (frame as u32).min(cols * rows - 1);
    let (cx, cy) = (idx % cols, idx / cols);
    mat.base_color_texture = Some(sheet.image.clone());
    mat.uv_transform = bevy::math::Affine2::from_scale_angle_translation(
        Vec2::new(1.0 / cols as f32, 1.0 / rows as f32),
        0.0,
        Vec2::new(cx as f32 / cols as f32, cy as f32 / rows as f32),
    );
}

fn animate_door_panels(
    time: Res<Time>,
    sprites: Res<DoorSprites>,
    states: Res<DoorStates>,
    mut panels: Query<&mut DoorPanel>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let dt = time.delta_secs();
    for mut p in &mut panels {
        let Some(set) = sprites.0.get(&p.set) else {
            continue;
        };
        let want = states.0.get(&p.id).copied().unwrap_or(false);
        let (op, cl) = (set.opening.cycle_seconds(), set.closing.cycle_seconds());
        p.anim.advance(want, dt, op, cl);
        let (sheet, frame) = match p.anim.phase {
            Phase::Closed => (&set.closed, 0),
            Phase::Open => (&set.open, 0),
            Phase::Opening => (&set.opening, frame_at(&set.opening.delays, p.anim.t)),
            Phase::Closing => (&set.closing, frame_at(&set.closing.delays, p.anim.t)),
        };
        if let Some(mut m) = materials.get_mut(&p.mesh_material) {
            set_frame(&mut m, sheet, frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_follow_delays_and_the_last_one_holds() {
        let d = [0.1, 0.05, 0.05];
        assert_eq!(frame_at(&d, 0.0), 0);
        assert_eq!(frame_at(&d, 0.099), 0);
        assert_eq!(frame_at(&d, 0.11), 1);
        assert_eq!(frame_at(&d, 0.17), 2);
        assert_eq!(frame_at(&d, 9.0), 2);
        assert_eq!(frame_at(&[], 1.0), 0);
    }

    #[test]
    fn a_door_plays_opening_then_rests_open_and_back() {
        let (op, cl) = (0.5, 0.5);
        let mut a = DoorAnim::new(false);
        assert_eq!(a.phase, Phase::Closed);
        a.advance(false, 1.0, op, cl);
        assert_eq!(a.phase, Phase::Closed, "без команды стоит закрытой");
        a.advance(true, 0.1, op, cl);
        assert_eq!(a.phase, Phase::Opening);
        assert!(!a.is_passable(), "в анимации проход ещё закрыт");
        a.advance(true, 0.5, op, cl);
        assert_eq!(a.phase, Phase::Open);
        assert!(a.is_passable());
        a.advance(false, 0.1, op, cl);
        assert_eq!(a.phase, Phase::Closing);
        a.advance(false, 1.0, op, cl);
        assert_eq!(a.phase, Phase::Closed);
    }

    #[test]
    fn reversing_midway_keeps_the_progress_instead_of_jumping() {
        let (op, cl) = (1.0, 1.0);
        let mut a = DoorAnim::new(false);
        a.advance(true, 0.0, op, cl);
        a.advance(true, 0.7, op, cl);
        assert_eq!(a.phase, Phase::Opening);
        // Открыта на 70 %: закрывается с 30 % пройденного пути закрытия.
        a.advance(false, 0.0, op, cl);
        assert_eq!(a.phase, Phase::Closing);
        assert!((a.t - 0.3).abs() < 1e-4, "{}", a.t);
        a.advance(true, 0.0, op, cl);
        assert_eq!(a.phase, Phase::Opening);
        assert!((a.t - 0.7).abs() < 1e-4, "{}", a.t);
        // Нулевая длительность не делит на ноль.
        let mut z = DoorAnim::new(false);
        z.advance(true, 0.0, 0.0, 0.0);
        z.advance(true, 0.0, 0.0, 0.0);
        assert_eq!(z.phase, Phase::Open);
    }
}
