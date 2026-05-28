//! Tests for the provider catalog loader. Pure file I/O + serde — no DB.

use roy_management::provider_catalog::{Catalog, CatalogError, DEFAULT_CATALOG_YAML};
use std::io::Write;

fn write_temp(content: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f
}

#[test]
fn missing_file_returns_empty_catalog() {
    let path = std::path::PathBuf::from("/tmp/does-not-exist-xxxxxxxx.yaml");
    let cat = Catalog::load_from(&path).unwrap();
    assert!(cat.providers().is_empty());
}

#[test]
fn empty_yaml_returns_empty_catalog() {
    let f = write_temp("[]\n");
    let cat = Catalog::load_from(f.path()).unwrap();
    assert!(cat.providers().is_empty());
}

#[test]
fn default_catalog_parses_and_contains_github() {
    // Reuses the embedded resource so the test fails if we ever break the
    // shipped sample.
    let providers: Vec<roy_management::provider_catalog::Provider> =
        serde_yaml::from_str(DEFAULT_CATALOG_YAML).unwrap();
    let github = providers.iter().find(|p| p.id == "github").unwrap();
    assert_eq!(github.command, "npx");
    assert_eq!(github.secrets[0].key, "GITHUB_PERSONAL_ACCESS_TOKEN");
}

#[test]
fn malformed_yaml_returns_parse_error() {
    let f = write_temp("not: valid: yaml: [\n");
    let err = Catalog::load_from(f.path()).unwrap_err();
    assert!(matches!(err, CatalogError::Parse { .. }), "{err}");
}

#[test]
fn missing_required_field_returns_parse_error() {
    // No `command` → serde_yaml deserialization fails before our schema
    // check; that's parse error, not schema.
    let f = write_temp("- id: x\n  name: x\n");
    let err = Catalog::load_from(f.path()).unwrap_err();
    assert!(matches!(err, CatalogError::Parse { .. }), "{err}");
}

#[test]
fn empty_id_returns_schema_error() {
    let f = write_temp("- id: ''\n  name: x\n  command: x\n");
    let err = Catalog::load_from(f.path()).unwrap_err();
    match err {
        CatalogError::Schema { reason, .. } => {
            assert!(reason.contains("`id` is empty"), "{reason}")
        }
        _ => panic!("expected Schema error, got {err}"),
    }
}

#[test]
fn duplicate_id_returns_schema_error() {
    let f = write_temp("- id: dup\n  name: A\n  command: x\n- id: dup\n  name: B\n  command: y\n");
    let err = Catalog::load_from(f.path()).unwrap_err();
    match err {
        CatalogError::Schema { reason, .. } => assert!(reason.contains("duplicate"), "{reason}"),
        _ => panic!("expected Schema error, got {err}"),
    }
}

#[test]
fn get_by_id_returns_the_right_provider() {
    let f = write_temp("- id: github\n  name: GitHub\n  command: npx\n  args: ['-y', '@x/y']\n");
    let cat = Catalog::load_from(f.path()).unwrap();
    assert_eq!(cat.get("github").unwrap().command, "npx");
    assert!(cat.get("nonexistent").is_none());
}
