use crate::product::agent::config::ConfigToml;
use crate::product::agent::config::models_json::ModelsJson;
use crate::product::agent::config::state_json::LHAStateJson;
use crate::product::agent::config::types::RawMcpServerConfig;
use crate::product::agent::features::FEATURES;
use schemars::r#gen::SchemaGenerator;
use schemars::r#gen::SchemaSettings;
use schemars::schema::InstanceType;
use schemars::schema::ObjectValidation;
use schemars::schema::RootSchema;
use schemars::schema::Schema;
use schemars::schema::SchemaObject;
use serde_json::Map;
use serde_json::Value;
use std::path::Path;

/// Schema for the `[features]` map with known + legacy keys only.
pub(crate) fn features_schema(schema_gen: &mut SchemaGenerator) -> Schema {
    let mut object = SchemaObject {
        instance_type: Some(InstanceType::Object.into()),
        ..Default::default()
    };

    let mut validation = ObjectValidation::default();
    for feature in FEATURES {
        validation
            .properties
            .insert(feature.key.to_string(), schema_gen.subschema_for::<bool>());
    }
    for legacy_key in crate::product::agent::features::legacy_feature_keys() {
        validation
            .properties
            .insert(legacy_key.to_string(), schema_gen.subschema_for::<bool>());
    }
    validation.additional_properties = Some(Box::new(Schema::Bool(false)));
    object.object = Some(Box::new(validation));

    Schema::Object(object)
}

/// Schema for the `[mcp_servers]` map using the raw input shape.
pub(crate) fn mcp_servers_schema(schema_gen: &mut SchemaGenerator) -> Schema {
    let mut object = SchemaObject {
        instance_type: Some(InstanceType::Object.into()),
        ..Default::default()
    };

    let validation = ObjectValidation {
        additional_properties: Some(Box::new(schema_gen.subschema_for::<RawMcpServerConfig>())),
        ..Default::default()
    };
    object.object = Some(Box::new(validation));

    Schema::Object(object)
}

/// Build the config schema for `config.toml`.
pub fn config_schema() -> RootSchema {
    SchemaSettings::draft07()
        .with(|settings| {
            settings.option_add_null_type = false;
        })
        .into_generator()
        .into_root_schema_for::<ConfigToml>()
}

/// Canonicalize a JSON value by sorting its keys.
fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            let mut sorted = Map::with_capacity(map.len());
            for (key, child) in entries {
                sorted.insert(key.clone(), canonicalize(child));
            }
            Value::Object(sorted)
        }
        _ => value.clone(),
    }
}

/// Render the config schema as pretty-printed JSON.
pub fn config_schema_json() -> anyhow::Result<Vec<u8>> {
    let schema = config_schema();
    let value = serde_json::to_value(schema)?;
    let value = canonicalize(&value);
    let json = serde_json::to_vec_pretty(&value)?;
    Ok(json)
}

fn schema_json_for<T: schemars::JsonSchema>() -> anyhow::Result<Vec<u8>> {
    let schema = SchemaSettings::draft07()
        .with(|settings| {
            settings.option_add_null_type = false;
        })
        .into_generator()
        .into_root_schema_for::<T>();
    let value = serde_json::to_value(schema)?;
    let value = canonicalize(&value);
    Ok(serde_json::to_vec_pretty(&value)?)
}

pub fn write_models_schema(out_path: &Path) -> anyhow::Result<()> {
    let json = schema_json_for::<ModelsJson>()?;
    std::fs::write(out_path, json)?;
    Ok(())
}

pub fn write_state_schema(out_path: &Path) -> anyhow::Result<()> {
    let json = schema_json_for::<LHAStateJson>()?;
    std::fs::write(out_path, json)?;
    Ok(())
}

/// Write the config schema fixture to disk.
pub fn write_config_schema(out_path: &Path) -> anyhow::Result<()> {
    let json = config_schema_json()?;
    std::fs::write(out_path, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::canonicalize;
    use super::config_schema_json;
    use super::schema_json_for;
    use crate::product::agent::config::models_json::ModelsJson;

    use similar::TextDiff;

    #[test]
    fn config_schema_matches_fixture() {
        let fixture_path = crate::test_support::cargo_bin::find_resource!(
            "product/agent_runtime/config.schema.json"
        )
        .expect("resolve config schema fixture path");
        let fixture = std::fs::read_to_string(fixture_path).expect("read config schema fixture");
        let fixture_value: serde_json::Value =
            serde_json::from_str(&fixture).expect("parse config schema fixture");
        let schema_json = config_schema_json().expect("serialize config schema");
        let schema_value: serde_json::Value =
            serde_json::from_slice(&schema_json).expect("decode schema json");
        let fixture_value = canonicalize(&fixture_value);
        let schema_value = canonicalize(&schema_value);
        if fixture_value != schema_value {
            let expected =
                serde_json::to_string_pretty(&fixture_value).expect("serialize fixture json");
            let actual =
                serde_json::to_string_pretty(&schema_value).expect("serialize schema json");
            let diff = TextDiff::from_lines(&expected, &actual)
                .unified_diff()
                .header("fixture", "generated")
                .to_string();
            panic!(
                "Current schema for `config.toml` doesn't match the fixture. \
Run `just write-config-schema` to overwrite with your changes.\n\n{diff}"
            );
        }
    }

    #[test]
    fn models_schema_matches_fixture() {
        let fixture_path = crate::test_support::cargo_bin::find_resource!(
            "product/agent_runtime/models.schema.json"
        )
        .expect("resolve models schema fixture path");
        let fixture = std::fs::read_to_string(fixture_path).expect("read models schema fixture");
        let fixture_value: serde_json::Value =
            serde_json::from_str(&fixture).expect("parse models schema fixture");
        let schema_json = schema_json_for::<ModelsJson>().expect("serialize models schema");
        let schema_value: serde_json::Value =
            serde_json::from_slice(&schema_json).expect("decode models schema json");
        let fixture_value = canonicalize(&fixture_value);
        let schema_value = canonicalize(&schema_value);
        if fixture_value != schema_value {
            let expected =
                serde_json::to_string_pretty(&fixture_value).expect("serialize fixture json");
            let actual =
                serde_json::to_string_pretty(&schema_value).expect("serialize schema json");
            let diff = TextDiff::from_lines(&expected, &actual)
                .unified_diff()
                .header("fixture", "generated")
                .to_string();
            panic!(
                "Current schema for `models.json` doesn't match the fixture. \
Run `just write-models-schema` to overwrite with your changes.\n\n{diff}"
            );
        }

        let band_properties = schema_value
            .pointer("/definitions/ModelPricingBand/properties")
            .expect("pricing band properties");
        for field in ["input", "cached_input", "output"] {
            assert_eq!(
                band_properties.pointer(&format!("/{field}/type")),
                Some(&serde_json::Value::String("number".to_string()))
            );
            assert_ne!(
                band_properties.pointer(&format!("/{field}/type")),
                Some(&serde_json::Value::String("integer".to_string()))
            );
            assert_ne!(
                band_properties.pointer(&format!("/{field}/format")),
                Some(&serde_json::Value::String("int64".to_string()))
            );
        }
    }
}
