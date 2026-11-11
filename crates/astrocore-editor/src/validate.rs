use astrocore_core::ObjectId;
use astrocore_math::GridCell;
use astrocore_spatial::{Diagnostic, Topology};
use astrocore_world::{FloorId, Placement, SlotKey, StructureKind, World};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FindingKind {
    /// Замкнутая комната без единого прохода.
    SealedRoom { min_cell: GridCell },
    /// Проём ведёт из комнаты в неё же.
    PortalInsideSingleRoom { element: ObjectId },
    /// Выход лестницы на верхнем этаже не опирается на пол.
    StairLandingWithoutFloor { element: ObjectId, cell: GridCell },
    /// Над лестницей осталось перекрытие верхнего этажа.
    StairBlockedByFloor { element: ObjectId, cell: GridCell },
    /// Этаж определён, но на нём ничего нет.
    EmptyFloor,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub severity: Severity,
    pub floor: Option<FloorId>,
    pub kind: FindingKind,
    pub message: String,
}

pub fn has_errors(findings: &[Finding]) -> bool {
    findings.iter().any(|f| f.severity == Severity::Error)
}

pub fn validate_map(world: &World) -> Vec<Finding> {
    let mut out = Vec::new();
    let floor_ids: Vec<FloorId> = world.floors().map(|f| f.id).collect();

    for &floor in &floor_ids {
        if world.floor_cell_bounds(floor).is_none() {
            out.push(Finding {
                severity: Severity::Warning,
                floor: Some(floor),
                kind: FindingKind::EmptyFloor,
                message: format!("этаж {} пуст", floor.0),
            });
            continue;
        }
        match Topology::build(world, floor) {
            Ok(topo) => {
                for d in topo.diagnostics() {
                    out.push(match d {
                        Diagnostic::SealedRoom { min_cell, .. } => Finding {
                            severity: Severity::Warning,
                            floor: Some(floor),
                            kind: FindingKind::SealedRoom { min_cell },
                            message: format!(
                                "замкнутая комната у ячейки ({}, {}) без прохода",
                                min_cell.x, min_cell.z
                            ),
                        },
                        Diagnostic::PortalInsideSingleRoom { element } => Finding {
                            severity: Severity::Warning,
                            floor: Some(floor),
                            kind: FindingKind::PortalInsideSingleRoom { element },
                            message: format!("проём {element} не соединяет разные помещения"),
                        },
                    });
                }
            }
            Err(e) => out.push(Finding {
                severity: Severity::Error,
                floor: Some(floor),
                kind: FindingKind::EmptyFloor,
                message: format!("топология этажа не построена: {e}"),
            }),
        }
    }

    for link in world.vertical_links() {
        let (to_floor, landing) = link.upper;
        if world
            .element_at(to_floor, SlotKey::FloorSlab(landing))
            .is_none()
        {
            out.push(Finding {
                severity: Severity::Warning,
                floor: Some(to_floor),
                kind: FindingKind::StairLandingWithoutFloor {
                    element: link.element,
                    cell: landing,
                },
                message: format!(
                    "выход лестницы на этаже {} не опирается на пол ({}, {})",
                    to_floor.0, landing.x, landing.z
                ),
            });
        }
        if let Some(e) = world.element(link.element)
            && let (StructureKind::Stair { .. } | StructureKind::Ramp { .. }, Placement::Run { .. }) =
                (e.kind, e.placement)
        {
            for cell in e.placement.run_cells() {
                if world
                    .element_at(to_floor, SlotKey::FloorSlab(cell))
                    .is_some()
                {
                    out.push(Finding {
                        severity: Severity::Error,
                        floor: Some(to_floor),
                        kind: FindingKind::StairBlockedByFloor {
                            element: link.element,
                            cell,
                        },
                        message: format!(
                            "над лестницей осталось перекрытие в ({}, {})",
                            cell.x, cell.z
                        ),
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.floor.cmp(&b.floor)));
    out
}
