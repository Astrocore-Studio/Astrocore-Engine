use std::collections::BTreeMap;

/// Область действия ресурса: от самой широкой к самой узкой (раздел 6 дизайн-документа).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// Всё приложение.
    Global,
    /// Один мир.
    World(String),
    /// Регион мира.
    Region { world: String, region: u32 },
    /// Игровая сессия.
    Session(u64),
    /// Соединение клиента.
    Client(u64),
}

/// Хранилище значений по областям. Поиск идёт от узкой области к широкой: значение клиента
/// перекрывает значение сессии, сессии — региона, региона — мира, мира — глобальное.
#[derive(Debug, Clone)]
pub struct ScopedResources<T> {
    values: BTreeMap<Scope, T>,
}

impl<T> Default for ScopedResources<T> {
    fn default() -> Self {
        Self {
            values: BTreeMap::new(),
        }
    }
}

/// Контекст запроса: какие области сейчас «активны».
#[derive(Debug, Clone, Default)]
pub struct ScopeChain {
    pub world: Option<String>,
    pub region: Option<u32>,
    pub session: Option<u64>,
    pub client: Option<u64>,
}

impl<T> ScopedResources<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, scope: Scope, value: T) -> Option<T> {
        self.values.insert(scope, value)
    }

    pub fn remove(&mut self, scope: &Scope) -> Option<T> {
        self.values.remove(scope)
    }

    pub fn get_exact(&self, scope: &Scope) -> Option<&T> {
        self.values.get(scope)
    }

    /// Самое узкое значение для цепочки областей.
    pub fn resolve(&self, chain: &ScopeChain) -> Option<&T> {
        if let Some(c) = chain.client
            && let Some(v) = self.values.get(&Scope::Client(c))
        {
            return Some(v);
        }
        if let Some(s) = chain.session
            && let Some(v) = self.values.get(&Scope::Session(s))
        {
            return Some(v);
        }
        if let (Some(w), Some(r)) = (&chain.world, chain.region)
            && let Some(v) = self.values.get(&Scope::Region {
                world: w.clone(),
                region: r,
            })
        {
            return Some(v);
        }
        if let Some(w) = &chain.world
            && let Some(v) = self.values.get(&Scope::World(w.clone()))
        {
            return Some(v);
        }
        self.values.get(&Scope::Global)
    }

    /// Убрать всё, что принадлежит закрывшимся сессии или клиенту (освобождение ресурсов).
    pub fn drop_session(&mut self, session: u64) {
        self.values.remove(&Scope::Session(session));
    }

    pub fn drop_client(&mut self, client: u64) {
        self.values.remove(&Scope::Client(client));
    }

    /// Убрать всё, что принадлежит миру (его регионам тоже).
    pub fn drop_world(&mut self, world: &str) {
        self.values.retain(|s, _| match s {
            Scope::World(w) => w != world,
            Scope::Region { world: w, .. } => w != world,
            _ => true,
        });
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain() -> ScopeChain {
        ScopeChain {
            world: Some("w".into()),
            region: Some(3),
            session: Some(10),
            client: Some(99),
        }
    }

    #[test]
    fn narrowest_scope_wins() {
        let mut r = ScopedResources::new();
        r.set(Scope::Global, "global");
        assert_eq!(r.resolve(&chain()), Some(&"global"));
        r.set(Scope::World("w".into()), "world");
        assert_eq!(r.resolve(&chain()), Some(&"world"));
        r.set(
            Scope::Region {
                world: "w".into(),
                region: 3,
            },
            "region",
        );
        assert_eq!(r.resolve(&chain()), Some(&"region"));
        r.set(Scope::Session(10), "session");
        assert_eq!(r.resolve(&chain()), Some(&"session"));
        r.set(Scope::Client(99), "client");
        assert_eq!(r.resolve(&chain()), Some(&"client"));
        // Другой клиент и другой регион видят более широкие значения.
        let other = ScopeChain {
            client: Some(5),
            region: Some(4),
            session: None,
            ..chain()
        };
        assert_eq!(r.resolve(&other), Some(&"world"));
        assert_eq!(r.resolve(&ScopeChain::default()), Some(&"global"));
    }

    #[test]
    fn closing_a_scope_releases_only_its_values() {
        let mut r = ScopedResources::new();
        r.set(Scope::World("a".into()), 1);
        r.set(
            Scope::Region {
                world: "a".into(),
                region: 1,
            },
            2,
        );
        r.set(Scope::World("b".into()), 3);
        r.set(Scope::Session(1), 4);
        r.set(Scope::Client(7), 5);
        r.drop_world("a");
        assert_eq!(r.len(), 3);
        r.drop_session(1);
        r.drop_client(7);
        assert_eq!(r.len(), 1);
        assert_eq!(r.get_exact(&Scope::World("b".into())), Some(&3));
        assert!(r.remove(&Scope::World("b".into())).is_some());
        assert!(r.is_empty());
    }
}
