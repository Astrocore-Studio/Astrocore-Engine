//! Шина событий: направленные и широковещательные события как в Robust.
//!
//! Образец — `Robust.Shared/GameObjects/EntityEventBus.*.cs` @ 1afd692. Событие поднимается
//! синхронно, обработчик может поднять другое событие или то же событие на другой сущности.
//! Направленное событие получают обработчики тех компонентов, которые есть у сущности.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

use bevy_ecs::component::{Component, ComponentId};
use bevy_ecs::entity::Entity;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use smallvec::SmallVec;

/// Обработчик направленного события: мир, сущность-адресат, событие.
pub type DirectedHandler<E> = Arc<dyn Fn(&mut World, Entity, &mut E) + Send + Sync>;
/// Обработчик широковещательного события.
pub type BroadcastHandler<E> = Arc<dyn Fn(&mut World, &mut E) + Send + Sync>;

/// Место подписки в порядке вызова: имя подписчика и подписчики, раньше или позже которых
/// он должен получить событие.
#[derive(Debug, Clone, Copy, Default)]
pub struct Order {
    pub label: &'static str,
    pub before: &'static [&'static str],
    pub after: &'static [&'static str],
}

impl Order {
    pub const fn label(label: &'static str) -> Self {
        Self {
            label,
            before: &[],
            after: &[],
        }
    }

    pub const fn before(mut self, labels: &'static [&'static str]) -> Self {
        self.before = labels;
        self
    }

    pub const fn after(mut self, labels: &'static [&'static str]) -> Self {
        self.after = labels;
        self
    }
}

struct Directed<E> {
    component: ComponentId,
    order: Order,
    handler: DirectedHandler<E>,
}

struct Broadcast<E> {
    order: Order,
    handler: BroadcastHandler<E>,
}

impl<E> Clone for Directed<E> {
    fn clone(&self) -> Self {
        Self {
            component: self.component,
            order: self.order,
            handler: self.handler.clone(),
        }
    }
}

impl<E> Clone for Broadcast<E> {
    fn clone(&self) -> Self {
        Self {
            order: self.order,
            handler: self.handler.clone(),
        }
    }
}

/// Подписки одного типа события, уже в порядке вызова.
struct Table<E> {
    directed: Vec<Directed<E>>,
    /// Компонент → индексы его подписок в `directed`.
    by_component: HashMap<ComponentId, SmallVec<[u32; 2]>>,
    broadcast: Vec<Broadcast<E>>,
}

impl<E> Clone for Table<E> {
    fn clone(&self) -> Self {
        Self {
            directed: self.directed.clone(),
            by_component: self.by_component.clone(),
            broadcast: self.broadcast.clone(),
        }
    }
}

impl<E> Default for Table<E> {
    fn default() -> Self {
        Self {
            directed: Vec::new(),
            by_component: HashMap::new(),
            broadcast: Vec::new(),
        }
    }
}

/// Упорядочивает подписки: сохраняет порядок регистрации, пока его не меняют `before`/`after`.
fn sort_by_order<T>(items: &mut Vec<T>, order_of: impl Fn(&T) -> Order, event: &str) {
    let orders: Vec<Order> = items.iter().map(&order_of).collect();
    if orders
        .iter()
        .all(|o| o.before.is_empty() && o.after.is_empty())
    {
        return;
    }
    let count = items.len();
    // must_follow[i] — индексы подписок, после которых идёт i.
    let mut must_follow: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (i, order) in orders.iter().enumerate() {
        for (j, other) in orders.iter().enumerate() {
            if i == j || other.label.is_empty() {
                continue;
            }
            if order.after.contains(&other.label) {
                must_follow[i].push(j);
            }
            if order.before.contains(&other.label) {
                must_follow[j].push(i);
            }
        }
    }
    let mut placed = vec![false; count];
    let mut sequence = Vec::with_capacity(count);
    while sequence.len() < count {
        let next = (0..count)
            .find(|&i| !placed[i] && must_follow[i].iter().all(|&j| placed[j]))
            .unwrap_or_else(|| {
                let cycle: Vec<&str> = (0..count)
                    .filter(|&i| !placed[i])
                    .map(|i| orders[i].label)
                    .collect();
                panic!("цикл в порядке подписок на {event}: {cycle:?}")
            });
        placed[next] = true;
        sequence.push(next);
    }
    let mut slots: Vec<Option<T>> = items.drain(..).map(Some).collect();
    items.extend(sequence.into_iter().filter_map(|i| slots[i].take()));
}

impl<E: 'static> Table<E> {
    fn reindex(&mut self) {
        let event = std::any::type_name::<E>();
        sort_by_order(&mut self.directed, |d| d.order, event);
        sort_by_order(&mut self.broadcast, |b| b.order, event);
        self.by_component.clear();
        for (index, entry) in self.directed.iter().enumerate() {
            self.by_component
                .entry(entry.component)
                .or_default()
                .push(index as u32);
        }
    }
}

/// Все подписки мира. Ресурс `World`.
#[derive(Resource, Default)]
pub struct EventBus {
    tables: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl EventBus {
    fn table<E: 'static>(&self) -> Option<Arc<Table<E>>> {
        let table = self.tables.get(&TypeId::of::<E>())?.clone();
        table.downcast::<Table<E>>().ok()
    }

    fn edit<E: 'static>(&mut self, edit: impl FnOnce(&mut Table<E>)) {
        let mut table = self
            .table::<E>()
            .map(|shared| (*shared).clone())
            .unwrap_or_default();
        edit(&mut table);
        table.reindex();
        self.tables.insert(TypeId::of::<E>(), Arc::new(table));
    }

    /// Подписывает обработчик на событие `E` у сущностей с компонентом `component`.
    pub fn subscribe_id<E: 'static>(
        &mut self,
        component: ComponentId,
        order: Order,
        handler: impl Fn(&mut World, Entity, &mut E) + Send + Sync + 'static,
    ) {
        self.edit::<E>(|table| {
            table.directed.push(Directed {
                component,
                order,
                handler: Arc::new(handler),
            });
        });
    }

    /// Подписывает обработчик на все события `E`, поднятые широковещательно.
    pub fn subscribe_broadcast<E: 'static>(
        &mut self,
        order: Order,
        handler: impl Fn(&mut World, &mut E) + Send + Sync + 'static,
    ) {
        self.edit::<E>(|table| {
            table.broadcast.push(Broadcast {
                order,
                handler: Arc::new(handler),
            });
        });
    }

    /// Есть ли у события `E` направленные подписки.
    pub fn has_directed<E: 'static>(&self) -> bool {
        self.table::<E>().is_some_and(|t| !t.directed.is_empty())
    }
}

/// События на `World`.
pub trait EventWorldExt {
    /// Подписка «компонент + событие»: `SubscribeLocalEvent<TComp, TEvent>` в Robust.
    fn subscribe<C: Component, E: 'static>(
        &mut self,
        handler: impl Fn(&mut World, Entity, &mut E) + Send + Sync + 'static,
    );

    /// Подписка с местом в порядке вызова.
    fn subscribe_ordered<C: Component, E: 'static>(
        &mut self,
        order: Order,
        handler: impl Fn(&mut World, Entity, &mut E) + Send + Sync + 'static,
    );

    /// Широковещательная подписка: `SubscribeLocalEvent<TEvent>` в Robust.
    fn subscribe_broadcast<E: 'static>(
        &mut self,
        handler: impl Fn(&mut World, &mut E) + Send + Sync + 'static,
    );

    /// Поднимает направленное событие на сущности: `RaiseLocalEvent(uid, ref ev)`.
    fn raise<E: 'static>(&mut self, target: Entity, event: &mut E);

    /// Поднимает направленное событие, затем широковещательное:
    /// `RaiseLocalEvent(uid, ref ev, broadcast: true)`.
    fn raise_and_broadcast<E: 'static>(&mut self, target: Entity, event: &mut E);

    /// Поднимает широковещательное событие: `RaiseLocalEvent(ref ev)`.
    fn raise_broadcast<E: 'static>(&mut self, event: &mut E);

    /// Поднимает событие только для подписок одного компонента: `RaiseComponentEvent`.
    fn raise_component<E: 'static>(
        &mut self,
        target: Entity,
        component: ComponentId,
        event: &mut E,
    );
}

fn bus(world: &mut World) -> bevy_ecs::world::Mut<'_, EventBus> {
    world.get_resource_or_insert_with(EventBus::default)
}

fn run_directed<E: 'static>(
    world: &mut World,
    table: &Table<E>,
    target: Entity,
    indices: &[u32],
    event: &mut E,
) {
    for (position, &index) in indices.iter().enumerate() {
        let entry = &table.directed[index as usize];
        // Предыдущий обработчик мог удалить сущность или компонент.
        if position > 0
            && !world
                .get_entity(target)
                .is_ok_and(|e| e.contains_id(entry.component))
        {
            continue;
        }
        (entry.handler)(world, target, event);
    }
}

impl EventWorldExt for World {
    fn subscribe<C: Component, E: 'static>(
        &mut self,
        handler: impl Fn(&mut World, Entity, &mut E) + Send + Sync + 'static,
    ) {
        self.subscribe_ordered::<C, E>(Order::default(), handler);
    }

    fn subscribe_ordered<C: Component, E: 'static>(
        &mut self,
        order: Order,
        handler: impl Fn(&mut World, Entity, &mut E) + Send + Sync + 'static,
    ) {
        let component = self.register_component::<C>();
        bus(self).subscribe_id(component, order, handler);
    }

    fn subscribe_broadcast<E: 'static>(
        &mut self,
        handler: impl Fn(&mut World, &mut E) + Send + Sync + 'static,
    ) {
        bus(self).subscribe_broadcast(Order::default(), handler);
    }

    fn raise<E: 'static>(&mut self, target: Entity, event: &mut E) {
        let Some(table) = self
            .get_resource::<EventBus>()
            .and_then(EventBus::table::<E>)
        else {
            return;
        };
        if table.directed.is_empty() {
            return;
        }
        let Ok(entity) = self.get_entity(target) else {
            return;
        };
        let mut indices: SmallVec<[u32; 8]> = SmallVec::new();
        for component in entity.archetype().components() {
            if let Some(found) = table.by_component.get(component) {
                indices.extend_from_slice(found);
            }
        }
        indices.sort_unstable();
        run_directed(self, &table, target, &indices, event);
    }

    fn raise_and_broadcast<E: 'static>(&mut self, target: Entity, event: &mut E) {
        self.raise(target, event);
        self.raise_broadcast(event);
    }

    fn raise_broadcast<E: 'static>(&mut self, event: &mut E) {
        let Some(table) = self
            .get_resource::<EventBus>()
            .and_then(EventBus::table::<E>)
        else {
            return;
        };
        for entry in &table.broadcast {
            (entry.handler)(self, event);
        }
    }

    fn raise_component<E: 'static>(
        &mut self,
        target: Entity,
        component: ComponentId,
        event: &mut E,
    ) {
        let Some(table) = self
            .get_resource::<EventBus>()
            .and_then(EventBus::table::<E>)
        else {
            return;
        };
        let Some(indices) = table.by_component.get(&component) else {
            return;
        };
        if !self
            .get_entity(target)
            .is_ok_and(|e| e.contains_id(component))
        {
            return;
        }
        run_directed(self, &table, target, indices, event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Component)]
    struct Door;
    #[derive(Component)]
    struct Lock;
    #[derive(Component, Default)]
    struct Log(Vec<&'static str>);

    struct Open {
        cancelled: bool,
    }
    struct Knock(u32);

    fn log(world: &mut World, target: Entity, line: &'static str) {
        world.get_mut::<Log>(target).unwrap().0.push(line);
    }

    #[test]
    fn directed_event_reaches_only_present_components() {
        let mut world = World::new();
        world.subscribe::<Door, Open>(|w, e, _| log(w, e, "door"));
        world.subscribe::<Lock, Open>(|w, e, ev: &mut Open| {
            ev.cancelled = true;
            log(w, e, "lock");
        });
        let plain = world.spawn((Door, Log::default())).id();
        let locked = world.spawn((Door, Lock, Log::default())).id();

        let mut open = Open { cancelled: false };
        world.raise(plain, &mut open);
        assert!(!open.cancelled);
        assert_eq!(world.get::<Log>(plain).unwrap().0, ["door"]);

        world.raise(locked, &mut open);
        assert!(open.cancelled);
        assert_eq!(world.get::<Log>(locked).unwrap().0, ["door", "lock"]);
    }

    #[test]
    fn order_constraints_override_registration_order() {
        let mut world = World::new();
        world.subscribe_ordered::<Door, Open>(Order::label("door"), |w, e, _| log(w, e, "door"));
        world.subscribe_ordered::<Lock, Open>(Order::label("lock").before(&["door"]), |w, e, _| {
            log(w, e, "lock")
        });
        world.subscribe_ordered::<Log, Open>(Order::label("log").after(&["door"]), |w, e, _| {
            log(w, e, "log")
        });
        let entity = world.spawn((Door, Lock, Log::default())).id();
        world.raise(entity, &mut Open { cancelled: false });
        assert_eq!(world.get::<Log>(entity).unwrap().0, ["lock", "door", "log"]);
    }

    #[test]
    fn nested_and_recursive_events_are_synchronous() {
        let mut world = World::new();
        world.subscribe::<Door, Knock>(|w, e, ev: &mut Knock| {
            if ev.0 > 0 {
                let mut inner = Knock(ev.0 - 1);
                w.raise(e, &mut inner);
                ev.0 += inner.0;
            } else {
                ev.0 = 100;
            }
        });
        let entity = world.spawn(Door).id();
        let mut knock = Knock(2);
        world.raise(entity, &mut knock);
        // 0 → 100; 1 → 1 + 100; 2 → 2 + 101.
        assert_eq!(knock.0, 103);
    }

    #[test]
    fn handler_may_remove_component_or_entity() {
        let mut world = World::new();
        world.subscribe::<Door, Open>(|w, e, _| {
            w.entity_mut(e).remove::<Lock>();
        });
        world.subscribe::<Lock, Open>(|_, _, ev: &mut Open| ev.cancelled = true);
        world.subscribe::<Door, Knock>(|w, e, _| {
            w.despawn(e);
        });
        world.subscribe::<Lock, Knock>(|_, _, ev: &mut Knock| ev.0 = 1);

        let entity = world.spawn((Door, Lock)).id();
        let mut open = Open { cancelled: false };
        world.raise(entity, &mut open);
        assert!(!open.cancelled);

        let entity = world.spawn((Door, Lock)).id();
        let mut knock = Knock(0);
        world.raise(entity, &mut knock);
        assert_eq!(knock.0, 0);
        world.raise(entity, &mut knock);
    }

    #[test]
    fn broadcast_and_component_events() {
        let mut world = World::new();
        world.subscribe_broadcast::<Knock>(|_, ev| ev.0 += 10);
        world.subscribe::<Door, Knock>(|_, _, ev: &mut Knock| ev.0 += 1);
        world.subscribe::<Lock, Knock>(|_, _, ev: &mut Knock| ev.0 += 100);
        let entity = world.spawn((Door, Lock)).id();

        let mut knock = Knock(0);
        world.raise_and_broadcast(entity, &mut knock);
        assert_eq!(knock.0, 111);

        let lock = world.component_id::<Lock>().unwrap();
        let mut knock = Knock(0);
        world.raise_component(entity, lock, &mut knock);
        assert_eq!(knock.0, 100);
    }

    #[test]
    #[should_panic(expected = "цикл в порядке подписок")]
    fn order_cycle_is_rejected() {
        let mut world = World::new();
        world.subscribe_ordered::<Door, Open>(Order::label("a").before(&["b"]), |_, _, _| {});
        world.subscribe_ordered::<Lock, Open>(Order::label("b").before(&["a"]), |_, _, _| {});
    }
}
