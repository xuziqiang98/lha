//! Applies agent-role configuration layers on top of an existing session config.
//!
//! Roles are selected at spawn time and are loaded with the same config machinery as
//! `config.toml`. This module resolves built-in and user-defined role files, inserts the role as a
//! high-precedence layer, and preserves the caller's current profile/provider/model selection.

use crate::config::AgentRoleConfig;
use crate::config::Config;
use crate::config::ConfigOverrides;
use crate::config::deserialize_config_toml_with_base;
use crate::config_loader::ConfigLayerEntry;
use crate::config_loader::ConfigLayerStack;
use crate::config_loader::ConfigLayerStackOrdering;
use crate::config_loader::resolve_relative_paths_in_config_toml;
use adam_app_server_protocol::ConfigLayerSource;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::LazyLock;
use toml::Value as TomlValue;

/// The role name used when a caller omits `agent_type`.
pub const DEFAULT_ROLE_NAME: &str = "default";
const AGENT_TYPE_UNAVAILABLE_ERROR: &str = "agent type is currently not available";

/// Applies a named role layer to `config` while preserving caller-owned model selection.
pub(crate) async fn apply_role_to_config(
    config: &mut Config,
    role_name: Option<&str>,
) -> Result<(), String> {
    let role_name = role_name.unwrap_or(DEFAULT_ROLE_NAME);
    let is_built_in = !config.agent_roles.contains_key(role_name);
    let (config_file, is_built_in) = resolve_role_config(config, role_name)
        .map(|role| (&role.config_file, is_built_in))
        .ok_or_else(|| format!("unknown agent_type '{role_name}'"))?;
    let Some(config_file) = config_file.as_ref() else {
        return Ok(());
    };

    let (role_config_contents, role_config_base) = if is_built_in {
        (
            built_in::config_file_contents(config_file)
                .map(str::to_owned)
                .ok_or_else(|| AGENT_TYPE_UNAVAILABLE_ERROR.to_string())?,
            config.adam_home.as_path(),
        )
    } else {
        (
            tokio::fs::read_to_string(config_file)
                .await
                .map_err(|_| AGENT_TYPE_UNAVAILABLE_ERROR.to_string())?,
            config_file
                .parent()
                .ok_or_else(|| AGENT_TYPE_UNAVAILABLE_ERROR.to_string())?,
        )
    };

    let role_config_toml: TomlValue = toml::from_str(&role_config_contents)
        .map_err(|_| AGENT_TYPE_UNAVAILABLE_ERROR.to_string())?;
    deserialize_config_toml_with_base(role_config_toml.clone(), role_config_base)
        .map_err(|_| AGENT_TYPE_UNAVAILABLE_ERROR.to_string())?;
    let role_layer_toml = resolve_relative_paths_in_config_toml(role_config_toml, role_config_base)
        .map_err(|_| AGENT_TYPE_UNAVAILABLE_ERROR.to_string())?;
    let role_selects_profile = role_layer_toml.get("profile").is_some();
    let preserve_current_profile = !role_selects_profile;

    let mut layers: Vec<ConfigLayerEntry> = config
        .config_layer_stack
        .get_layers(ConfigLayerStackOrdering::LowestPrecedenceFirst, true)
        .into_iter()
        .cloned()
        .collect();
    let layer = ConfigLayerEntry::new(ConfigLayerSource::SessionFlags, role_layer_toml);
    let insertion_index =
        layers.partition_point(|existing_layer| existing_layer.name <= layer.name);
    layers.insert(insertion_index, layer);

    let config_layer_stack = ConfigLayerStack::new(
        layers,
        config.config_layer_stack.requirements().clone(),
        config.config_layer_stack.requirements_toml().clone(),
    )
    .map_err(|_| AGENT_TYPE_UNAVAILABLE_ERROR.to_string())?;

    let merged_toml = config_layer_stack.effective_config();
    let merged_config = deserialize_config_toml_with_base(merged_toml, &config.adam_home)
        .map_err(|_| AGENT_TYPE_UNAVAILABLE_ERROR.to_string())?;
    let next_config = Config::load_config_with_layer_stack(
        merged_config,
        ConfigOverrides {
            cwd: Some(config.cwd.clone()),
            model: config.model.clone(),
            model_provider: Some(config.model_provider_id.clone()),
            config_profile: preserve_current_profile
                .then(|| config.active_profile.clone())
                .flatten(),
            codex_linux_sandbox_exe: config.codex_linux_sandbox_exe.clone(),
            ..Default::default()
        },
        config.adam_home.clone(),
        config_layer_stack,
        Some(config.provider_config_required),
    )
    .map_err(|_| AGENT_TYPE_UNAVAILABLE_ERROR.to_string())?;
    *config = next_config;

    Ok(())
}

pub(crate) fn resolve_role_config<'a>(
    config: &'a Config,
    role_name: &str,
) -> Option<&'a AgentRoleConfig> {
    config
        .agent_roles
        .get(role_name)
        .or_else(|| built_in::configs().get(role_name))
}

pub(crate) mod spawn_tool_spec {
    use super::*;

    /// Builds the spawn-agent tool description text from built-in and configured roles.
    pub(crate) fn build(user_defined_agent_roles: &BTreeMap<String, AgentRoleConfig>) -> String {
        let built_in_roles = built_in::configs();
        build_from_configs(built_in_roles, user_defined_agent_roles)
    }

    fn build_from_configs(
        built_in_roles: &BTreeMap<String, AgentRoleConfig>,
        user_defined_roles: &BTreeMap<String, AgentRoleConfig>,
    ) -> String {
        let mut seen = BTreeSet::new();
        let mut formatted_roles = Vec::new();
        for (name, declaration) in user_defined_roles {
            if seen.insert(name.as_str()) {
                formatted_roles.push(format_role(name, declaration));
            }
        }
        for (name, declaration) in built_in_roles {
            if seen.insert(name.as_str()) {
                formatted_roles.push(format_role(name, declaration));
            }
        }

        format!(
            r#"Optional type name for the new agent. If omitted, `{DEFAULT_ROLE_NAME}` is used.
Available roles:
{}
            "#,
            formatted_roles.join("\n"),
        )
    }

    fn format_role(name: &str, declaration: &AgentRoleConfig) -> String {
        if let Some(description) = &declaration.description {
            format!("{name}: {{\n{description}\n}}")
        } else {
            format!("{name}: no description")
        }
    }
}

mod built_in {
    use super::*;

    /// Returns the cached built-in role declarations defined in this module.
    pub(super) fn configs() -> &'static BTreeMap<String, AgentRoleConfig> {
        static CONFIG: LazyLock<BTreeMap<String, AgentRoleConfig>> = LazyLock::new(|| {
            BTreeMap::from([
                (
                    DEFAULT_ROLE_NAME.to_string(),
                    AgentRoleConfig {
                        description: Some("Default agent.".to_string()),
                        config_file: None,
                        nickname_candidates: None,
                    },
                ),
                (
                    "explorer".to_string(),
                    AgentRoleConfig {
                        description: Some(r#"Use `explorer` for specific codebase questions.
Explorers are fast and authoritative.
They must be used to ask specific, well-scoped questions on the codebase.
Rules:
- In order to avoid redundant work, you should avoid exploring the same problem that explorers have already covered. Typically, you should trust the explorer results without additional verification. You are still allowed to inspect the code yourself to gain the needed context!
- You are encouraged to spawn up multiple explorers in parallel when you have multiple distinct questions to ask about the codebase that can be answered independently. This allows you to get more information faster without waiting for one question to finish before asking the next. While waiting for the explorer results, you can continue working on other local tasks that do not depend on those results. This parallelism is a key advantage of delegation, so use it whenever you have multiple questions to ask.
- Reuse existing explorers for related questions."#.to_string()),
                        config_file: Some("explorer.toml".to_string().into()),
                        nickname_candidates: None,
                    },
                ),
                (
                    "worker".to_string(),
                    AgentRoleConfig {
                        description: Some(r#"Use for execution and production work.
Typical tasks:
- Implement part of a feature
- Fix tests or bugs
- Split large refactors into independent chunks
Rules:
- Explicitly assign **ownership** of the task (files / responsibility). When the subtask involves code changes, you should clearly specify which files or modules the worker is responsible for. This helps avoid merge conflicts and ensures accountability. For example, you can say "Worker 1 is responsible for updating the authentication module, while Worker 2 will handle the database layer." By defining clear ownership, you can delegate more effectively and reduce coordination overhead.
- Always tell workers they are **not alone in the codebase**, and they should not revert the edits made by others, and they should adjust their implementation to accommodate the changes made by others. This is important because there may be multiple workers making changes in parallel, and they need to be aware of each other's work to avoid conflicts and ensure a cohesive final product."#.to_string()),
                        config_file: None,
                        nickname_candidates: None,
                    },
                ),
            ])
        });
        &CONFIG
    }

    /// Resolves a built-in role `config_file` path to embedded content.
    pub(super) fn config_file_contents(path: &Path) -> Option<&'static str> {
        const EXPLORER: &str = include_str!("builtins/explorer.toml");
        const AWAITER: &str = include_str!("builtins/awaiter.toml");
        match path.to_str()? {
            "explorer.toml" => Some(EXPLORER),
            "awaiter.toml" => Some(AWAITER),
            _ => None,
        }
    }
}
