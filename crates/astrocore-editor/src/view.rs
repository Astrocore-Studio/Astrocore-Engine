use std::collections::BTreeSet;

use astrocore_world::{FloorId, StructureElement, StructureKind, World};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloorView {
    /// Здание целиком.
    All,
    /// Срез: показаны этаж `top` и всё, что ниже; выше — скрыто; потолок самого `top` тоже скрыт,
    /// чтобы заглянуть внутрь. Верхние перекрытия не удаляются из мира.
    Slice { top: FloorId },
    /// Рентген: этаж `focus` непрозрачен, остальные просвечивают.
    XRay { focus: FloorId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Opaque,
    Translucent,
    Hidden,
}

pub fn visibility(world: &World, view: FloorView, e: &StructureElement) -> Visibility {
    match view {
        FloorView::All => Visibility::Opaque,
        FloorView::Slice { top } => {
            let (Some(f), Some(t)) = (world.floor(e.floor), world.floor(top)) else {
                return Visibility::Hidden;
            };
            if f.base_y > t.base_y || (e.floor == top && matches!(e.kind, StructureKind::Ceiling)) {
                Visibility::Hidden
            } else {
                Visibility::Opaque
            }
        }
        FloorView::XRay { focus } => {
            if e.floor == focus {
                Visibility::Opaque
            } else {
                Visibility::Translucent
            }
        }
    }
}

/// Этажи, которые вообще нужно рисовать при данном режиме.
pub fn visible_floors(world: &World, view: FloorView) -> BTreeSet<FloorId> {
    match view {
        FloorView::All | FloorView::XRay { .. } => world.floors().map(|f| f.id).collect(),
        FloorView::Slice { top } => match world.floor(top) {
            Some(t) => world
                .floors()
                .filter(|f| f.base_y <= t.base_y)
                .map(|f| f.id)
                .collect(),
            None => BTreeSet::new(),
        },
    }
}
