//! Дерево данных Astrocore: общий вид YAML (формат контента SS14) и RON (формат собственного
//! контента), мост к `serde` и запись обратно в оба формата.

mod de;
mod node;
mod ron;
mod ser;
mod write;
mod yaml;

pub use de::{UnknownField, from_node, from_node_checked};
pub use node::{DataError, DataMap, DataNode, Scalar, TYPE_TAG_PREFIX, Value};
pub use ron::parse_ron;
pub use ser::to_node;
pub use write::{to_ron_string, to_yaml_string};
pub use yaml::{parse_yaml, parse_yaml_documents};

/// Формат файла данных по расширению имени.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataFormat {
    Yaml,
    Ron,
}

impl DataFormat {
    /// `.yml` и `.yaml` — YAML, `.ron` — RON.
    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "yml" | "yaml" => Some(Self::Yaml),
            "ron" => Some(Self::Ron),
            _ => None,
        }
    }

    pub fn parse(self, text: &str) -> Result<DataNode, DataError> {
        match self {
            Self::Yaml => parse_yaml(text),
            Self::Ron => parse_ron(text),
        }
    }

    pub fn write(self, node: &DataNode) -> String {
        match self {
            Self::Yaml => to_yaml_string(node),
            Self::Ron => to_ron_string(node),
        }
    }
}
