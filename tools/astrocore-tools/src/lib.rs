pub mod bench;

use std::fs;
use std::path::Path;

use astrocore_assets::AssetManifest;
use astrocore_editor::project::{Project, ProjectError};
use astrocore_editor::validate::{Finding, has_errors, validate_map};
use astrocore_world_compiler::{
    BundleManifest, CompileError, CompileInput, compile, verify_compatible,
};

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("проект: {0}")]
    Project(#[from] ProjectError),
    #[error("ввод-вывод: {0}")]
    Io(#[from] std::io::Error),
    #[error("компиляция: {0}")]
    Compile(#[from] CompileError),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("пакеты несовместимы: {0}")]
    Compat(String),
    #[error("карта содержит ошибки ({0})")]
    Invalid(usize),
}

/// Создать демонстрационный проект (здание вертикального среза).
pub fn make_demo_project(path: &Path) -> Result<(), ToolError> {
    let slice =
        astrocore_vertical_slice::Slice::build().map_err(|e| ProjectError::World(e.to_string()))?;
    Project::new("vertical-slice", slice.world).save(path)?;
    Ok(())
}

/// Проверка карты: загружает проект и возвращает находки. Ошибки (не предупреждения) дают `Err`.
pub fn validate_project(path: &Path) -> Result<Vec<Finding>, ToolError> {
    let project = Project::load(path)?;
    let findings = validate_map(&project.world);
    if has_errors(&findings) {
        let errors = findings
            .iter()
            .filter(|f| f.severity == astrocore_editor::validate::Severity::Error)
            .count();
        return Err(ToolError::Invalid(errors));
    }
    Ok(findings)
}

/// Скомпилировать проект в каталог: `client.acpk`, `server.acpk`, `bundle.json`.
/// Перед записью проверяется совместимость половин и политика лицензий не применяется
/// (она относится к публичному экспорту ассетов).
pub fn compile_project(
    path: &Path,
    out_dir: &Path,
    game_id: &str,
) -> Result<BundleManifest, ToolError> {
    let project = Project::load(path)?;
    let assets = AssetManifest::new();
    let map = compile(&CompileInput {
        world: &project.world,
        game_id,
        assets: &assets,
        extra_dependencies: &[],
        require_materials_in_manifest: false,
    })?;
    verify_compatible(&map.client, &map.server, &map.manifest, game_id)
        .map_err(|e| ToolError::Compat(e.to_string()))?;
    fs::create_dir_all(out_dir)?;
    fs::write(out_dir.join("client.acpk"), &map.client)?;
    fs::write(out_dir.join("server.acpk"), &map.server)?;
    fs::write(
        out_dir.join("bundle.json"),
        serde_json::to_string_pretty(&map.manifest)?,
    )?;
    Ok(map.manifest)
}

/// Проверить ранее собранный каталог пакетов.
pub fn verify_bundle(dir: &Path, game_id: &str) -> Result<BundleManifest, ToolError> {
    let manifest: BundleManifest = serde_json::from_slice(&fs::read(dir.join("bundle.json"))?)?;
    let client = fs::read(dir.join("client.acpk"))?;
    let server = fs::read(dir.join("server.acpk"))?;
    verify_compatible(&client, &server, &manifest, game_id)
        .map_err(|e| ToolError::Compat(e.to_string()))?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_project_validates_compiles_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("demo.acproj");
        make_demo_project(&project).unwrap();
        let findings = validate_project(&project).unwrap();
        // Верхние этажи — замкнутые комнаты без прохода (предупреждения), ошибок нет.
        assert!(!findings.is_empty());
        let out = dir.path().join("out");
        let manifest = compile_project(&project, &out, "demo").unwrap();
        assert_eq!(manifest.world_id, "vertical-slice");
        let again = verify_bundle(&out, "demo").unwrap();
        assert_eq!(again, manifest);
        assert!(verify_bundle(&out, "other").is_err());
        // Повреждённый пакет не проходит проверку.
        let mut bytes = std::fs::read(out.join("client.acpk")).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xFF;
        std::fs::write(out.join("client.acpk"), bytes).unwrap();
        assert!(matches!(
            verify_bundle(&out, "demo"),
            Err(ToolError::Compat(_))
        ));
    }

    #[test]
    fn validator_fails_on_errors_and_on_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        // Лестница с оставленным над ней перекрытием — ошибка валидатора.
        let mut s = astrocore_editor::EditSession::new(
            astrocore_vertical_slice::Slice::build().unwrap().world,
        );
        let (id, cmd) = astrocore_editor::tools::add_floor_above(s.world(), "лишний", 3.0);
        s.execute("этаж", vec![cmd]).unwrap();
        let mut ids = astrocore_editor::tools::SequentialIds(900_000);
        let mat = astrocore_core::PrototypeId::new("t:m").unwrap();
        let slab = astrocore_editor::tools::room_rect(
            s.world(),
            &mut ids,
            id,
            astrocore_math::GridCell::new(30, 5),
            astrocore_math::GridCell::new(33, 5),
            &mat,
            0.2,
            true,
            false,
        )
        .unwrap();
        s.execute("перекрытие", slab).unwrap();
        // Ставим лестницу с верхнего этажа 2 на лишний этаж без пробивки поверх существующего перекрытия.
        let stair = astrocore_editor::tools::stair_between(
            s.world(),
            &mut ids,
            astrocore_world::FloorId(2),
            id,
            astrocore_math::GridCell::new(30, 5),
            astrocore_math::Facing::East,
            4,
            false,
            &mat,
            false,
        )
        .unwrap();
        s.execute("лестница", stair).unwrap();
        let path = dir.path().join("bad.acproj");
        Project::new("bad", s.into_world()).save(&path).unwrap();
        assert!(matches!(validate_project(&path), Err(ToolError::Invalid(n)) if n >= 1));
        assert!(matches!(
            validate_project(&dir.path().join("none")),
            Err(ToolError::Project(_))
        ));
    }
}
