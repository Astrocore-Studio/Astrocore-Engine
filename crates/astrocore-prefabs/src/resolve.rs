use std::collections::{BTreeMap, BTreeSet};

use astrocore_core::{ObjectId, PrefabInstanceId, PrefabNodeId, PrototypeId};
use astrocore_math::GridTransform;
use astrocore_world::{FloorId, StructureElement};

use crate::model::*;
use crate::registry::{MAX_PREFAB_DEPTH, PrefabRegistry};

/// Элемент после разворачивания наследования и вложенности (ещё без привязки к миру).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedNode {
    pub path: NodePath,
    pub template: ElementTemplate,
    /// Преобразования вложенности, от внешнего к внутреннему: (сетка, смещение этажа).
    pub chain: Vec<(GridTransform, i32)>,
}

/// Детерминированный `ObjectId` узла экземпляра.
pub fn object_id_for(instance: PrefabInstanceId, path: &[PrefabNodeId]) -> ObjectId {
    let mut name = Vec::with_capacity(path.len() * 16);
    for node in path {
        name.extend_from_slice(node.as_uuid().as_bytes());
    }
    ObjectId::derive(&ObjectId::from_uuid(instance.as_uuid()), &name)
}

/// Узлы префаба с учётом наследования и переопределений определения.
fn effective_nodes(
    reg: &PrefabRegistry,
    id: &PrototypeId,
    inherit_stack: &mut Vec<PrototypeId>,
) -> Result<Vec<PrefabNode>, PrefabError> {
    if inherit_stack.contains(id) {
        let chain: Vec<String> = inherit_stack
            .iter()
            .chain([id])
            .map(ToString::to_string)
            .collect();
        return Err(PrefabError::Cycle(chain.join(" → ")));
    }
    if inherit_stack.len() >= MAX_PREFAB_DEPTH {
        return Err(PrefabError::TooDeep(MAX_PREFAB_DEPTH));
    }
    let def = reg
        .get(id)
        .ok_or_else(|| PrefabError::Missing(id.clone()))?;
    inherit_stack.push(id.clone());
    let mut nodes = match &def.parent {
        Some(parent) => effective_nodes(reg, parent, inherit_stack)?,
        None => Vec::new(),
    };
    inherit_stack.pop();

    for own in &def.nodes {
        match nodes.iter_mut().find(|n| n.id == own.id) {
            Some(slot) => *slot = own.clone(),
            None => nodes.push(own.clone()),
        }
    }
    nodes.retain_mut(|n| match def.overrides.get(&n.id) {
        Some(o) if o.removed => false,
        Some(o) => {
            apply_override(&mut n.content, o);
            true
        }
        None => true,
    });
    Ok(nodes)
}

fn apply_override(content: &mut NodeContent, o: &NodeOverride) {
    if let NodeContent::Element(t) = content {
        if let Some(m) = &o.material {
            t.material = m.clone();
        }
        if let Some(th) = o.thickness {
            t.thickness = th;
        }
    }
}

fn walk(
    reg: &PrefabRegistry,
    id: &PrototypeId,
    prefix: &NodePath,
    chain: &[(GridTransform, i32)],
    nest_stack: &mut Vec<PrototypeId>,
    out: &mut Vec<ResolvedNode>,
) -> Result<(), PrefabError> {
    if nest_stack.contains(id) {
        let list: Vec<String> = nest_stack
            .iter()
            .chain([id])
            .map(ToString::to_string)
            .collect();
        return Err(PrefabError::Cycle(list.join(" → ")));
    }
    if nest_stack.len() >= MAX_PREFAB_DEPTH {
        return Err(PrefabError::TooDeep(MAX_PREFAB_DEPTH));
    }
    let nodes = effective_nodes(reg, id, &mut Vec::new())?;
    nest_stack.push(id.clone());
    for node in nodes {
        let mut path = prefix.clone();
        path.push(node.id);
        match node.content {
            NodeContent::Element(template) => {
                out.push(ResolvedNode {
                    path,
                    template,
                    chain: chain.to_vec(),
                });
            }
            NodeContent::Child(child) => {
                let def = reg
                    .get(&child.prefab)
                    .ok_or_else(|| PrefabError::Missing(child.prefab.clone()))?;
                if let Some(required) = child.required_version
                    && required != def.version
                {
                    return Err(PrefabError::VersionMismatch {
                        prefab: id.clone(),
                        child: child.prefab.clone(),
                        required,
                        actual: def.version,
                    });
                }
                let mut inner = chain.to_vec();
                inner.push((child.transform, child.floor_offset));
                walk(reg, &child.prefab, &path, &inner, nest_stack, out)?;
            }
        }
    }
    nest_stack.pop();
    Ok(())
}

/// Развернуть префаб в плоский список элементов (без привязки к экземпляру).
pub fn resolve(
    reg: &PrefabRegistry,
    prefab: &PrototypeId,
) -> Result<Vec<ResolvedNode>, PrefabError> {
    let mut out = Vec::new();
    walk(reg, prefab, &Vec::new(), &[], &mut Vec::new(), &mut out)?;
    Ok(out)
}

/// Элементы мира, которые должен порождать экземпляр. Применяет преобразования
/// вложенности и экземпляра, переопределения экземпляра и выводит `ObjectId`.
pub fn materialize(
    reg: &PrefabRegistry,
    instance: &PrefabInstance,
) -> Result<BTreeMap<ObjectId, StructureElement>, PrefabError> {
    let mut out = BTreeMap::new();
    let mut slots = BTreeSet::new();
    let mut chain_root = vec![(instance.transform, instance.base_floor)];
    let mut nodes = Vec::new();
    walk(
        reg,
        &instance.prefab,
        &Vec::new(),
        &chain_root,
        &mut Vec::new(),
        &mut nodes,
    )?;
    chain_root.clear();

    for mut node in nodes {
        if let Some(o) = instance.overrides.iter().find(|o| o.path == node.path) {
            if o.value.removed {
                continue;
            }
            if let Some(m) = &o.value.material {
                node.template.material = m.clone();
            }
            if let Some(t) = o.value.thickness {
                node.template.thickness = t;
            }
        }
        let shift: i32 = node.chain.iter().map(|(_, f)| f).sum();
        let mut placement = node.template.placement;
        for (t, _) in node.chain.iter().rev() {
            placement = placement.apply(*t);
        }
        let element = StructureElement {
            id: object_id_for(instance.id, &node.path),
            kind: node.template.kind.to_world(shift),
            placement: placement.into_world(),
            floor: FloorId(shift + node.template.floor_offset),
            material: node.template.material,
            thickness: node.template.thickness,
        };
        for slot in element.slots() {
            if !slots.insert(slot) {
                return Err(PrefabError::SlotCollision(format!("{slot:?}")));
            }
        }
        out.insert(element.id, element);
    }
    Ok(out)
}
