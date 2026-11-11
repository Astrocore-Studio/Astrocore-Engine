use astrocore_spatial_topology_example::{breach_changes, floor_report, mark_zone};
use astrocore_vertical_slice::Slice;
use astrocore_world::FloorId;

fn main() {
    let mut slice = Slice::build().expect("демо-здание строится");
    let r = floor_report(&slice, FloorId(0)).expect("топология этажа");
    println!(
        "этаж 0: внутренних комнат {}, порталов {}; по воздуху достижимо {} (двери закрыты) / {} (открыты)",
        r.interior_rooms,
        r.portals,
        r.reach_closed.len(),
        r.reach_open.len()
    );
    let changes = breach_changes(&mut slice).expect("пролом");
    println!("после пролома стены: {} изменений топологии", changes.len());
    for c in &changes {
        println!("  {c:?}");
    }
    let id = mark_zone(&mut slice, "двор").expect("зона");
    println!(
        "авторская зона {id} добавлена; объектов в мире: {}",
        slice.world.authored_count()
    );
}
