//! Centralized feature flags and metadata.
//!
//! This module defines a small set of toggles that gate experimental and
//! optional behavior across the codebase. Instead of wiring individual
//! booleans through multiple types, call sites consult a single `Features`
//! container attached to `Config`.

use crate::product::agent::config::CONFIG_TOML_FILE;
use crate::product::agent::config::Config;
use crate::product::agent::config::ConfigToml;
use crate::product::agent::config::profile::ConfigProfile;
use crate::product::agent::protocol::Event;
use crate::product::agent::protocol::EventMsg;
use crate::product::agent::protocol::WarningEvent;
use crate::product::otel::OtelManager;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use toml::Value as TomlValue;

mod legacy;
pub(crate) use legacy::LegacyFeatureToggles;
pub(crate) use legacy::legacy_feature_keys;

/// High-level lifecycle stage for a feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Features that are still under development, not ready for external use
    UnderDevelopment,
    /// Experimental features made available to users through the `/experimental` menu
    Experimental {
        name: &'static str,
        menu_description: &'static str,
        announcement: &'static str,
    },
    /// Stable features. The feature flag is kept for ad-hoc enabling/disabling
    Stable,
    /// Deprecated feature that should not be used anymore.
    Deprecated,
    /// The feature flag is useless but kept for backward compatibility reason.
    Removed,
}

impl Stage {
    pub fn experimental_menu_name(self) -> Option<&'static str> {
        match self {
            Stage::Experimental { name, .. } => Some(name),
            _ => None,
        }
    }

    pub fn experimental_menu_description(self) -> Option<&'static str> {
        match self {
            Stage::Experimental {
                menu_description, ..
            } => Some(menu_description),
            _ => None,
        }
    }

    pub fn experimental_announcement(self) -> Option<&'static str> {
        match self {
            Stage::Experimental { announcement, .. } => Some(announcement),
            _ => None,
        }
    }
}

/// Unique features toggled via configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Feature {
    // Stable.
    /// Create a ghost commit at each turn.
    GhostCommit,
    /// Enable the default shell tool.
    ShellTool,
    /// Enable long-running programmer goals.
    Goals,
    /// Allow LHA to create and use local memories from prior conversations.
    MemoryTool,

    // Experimental
    /// Use the single unified PTY-backed exec tool.
    UnifiedExec,
    /// Include the freeform apply_patch tool.
    ApplyPatchFreeform,
    /// Allow the model to request web searches that fetch live content.
    WebSearchRequest,
    /// Allow the model to request web searches that fetch cached content.
    /// Takes precedence over `WebSearchRequest`.
    WebSearchCached,
    /// Gate the execpolicy enforcement for shell/unified exec.
    ExecPolicy,
    /// Allow the model to request approval and propose exec rules.
    RequestRule,
    /// Enable Windows sandbox (restricted token) on Windows.
    WindowsSandbox,
    /// Use the elevated Windows sandbox pipeline (setup + runner).
    WindowsSandboxElevated,
    /// Remote compaction enabled.
    RemoteCompaction,
    /// Refresh remote models and emit AppReady once the list is available.
    RemoteModels,
    /// Experimental shell snapshotting.
    ShellSnapshot,
    /// Enable runtime metrics snapshots via a manual reader.
    RuntimeMetrics,
    /// Persist rollout metadata to a local SQLite database.
    Sqlite,
    /// Append additional AGENTS.md guidance to user instructions.
    ChildAgentsMd,
    /// Enforce UTF8 output in Powershell.
    PowershellUtf8,
    /// Compress request bodies (zstd) when sending streaming requests to codex-backend.
    EnableRequestCompression,
    /// Force HTTP/1.1 for model streaming requests.
    ForceHttp1Streaming,
    /// Enable one-shot delegated agent job tools.
    AgentJobs,
    /// Enable apps.
    Apps,
    /// Allow prompting and installing missing MCP dependencies.
    SkillMcpDependencyInstall,
    /// Prompt for missing skill env var dependencies.
    SkillEnvVarDependencyPrompt,
    /// Steer feature flag - when enabled, Enter submits immediately instead of queuing.
    Steer,
    /// Backfill the latest proposed plan into compacted history.
    BackfillCompactPlanContext,
    /// Enable identities.
    Identities,
    /// Enable personality selection in the TUI.
    Personality,
    /// Prevent the computer from sleeping while LHA is running a turn.
    PreventIdleSleep,
    /// Use the Responses API WebSocket transport for OpenAI by default.
    ResponsesWebsockets,
    /// Slim large old tool results in model requests without rewriting history.
    InputSlimming,
}

impl Feature {
    pub fn key(self) -> &'static str {
        self.info().key
    }

    pub fn stage(self) -> Stage {
        self.info().stage
    }

    pub fn default_enabled(self) -> bool {
        self.info().default_enabled
    }

    fn info(self) -> &'static FeatureSpec {
        FEATURES
            .iter()
            .find(|spec| spec.id == self)
            .unwrap_or_else(|| unreachable!("missing FeatureSpec for {:?}", self))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LegacyFeatureUsage {
    pub alias: String,
    pub feature: Feature,
    pub summary: String,
    pub details: Option<String>,
}

/// Holds the effective set of enabled features.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Features {
    enabled: BTreeSet<Feature>,
    legacy_usages: BTreeSet<LegacyFeatureUsage>,
}

#[derive(Debug, Clone, Default)]
pub struct FeatureOverrides {
    pub include_apply_patch_tool: Option<bool>,
    pub web_search_request: Option<bool>,
}

impl FeatureOverrides {
    fn apply(self, features: &mut Features) {
        LegacyFeatureToggles {
            include_apply_patch_tool: self.include_apply_patch_tool,
            tools_web_search: self.web_search_request,
            ..Default::default()
        }
        .apply(features);
    }
}

impl Features {
    /// Starts with built-in defaults.
    pub fn with_defaults() -> Self {
        let mut set = BTreeSet::new();
        for spec in FEATURES {
            if spec.default_enabled {
                set.insert(spec.id);
            }
        }
        Self {
            enabled: set,
            legacy_usages: BTreeSet::new(),
        }
    }

    pub fn enabled(&self, f: Feature) -> bool {
        self.enabled.contains(&f)
    }

    pub fn enable(&mut self, f: Feature) -> &mut Self {
        self.enabled.insert(f);
        self
    }

    pub fn disable(&mut self, f: Feature) -> &mut Self {
        self.enabled.remove(&f);
        self
    }

    pub fn record_legacy_usage_force(&mut self, alias: &str, feature: Feature) {
        let (summary, details) = legacy_usage_notice(alias, feature);
        self.legacy_usages.insert(LegacyFeatureUsage {
            alias: alias.to_string(),
            feature,
            summary,
            details,
        });
    }

    pub fn record_legacy_usage(&mut self, alias: &str, feature: Feature) {
        if alias == feature.key() {
            return;
        }
        self.record_legacy_usage_force(alias, feature);
    }

    pub fn legacy_feature_usages(&self) -> impl Iterator<Item = &LegacyFeatureUsage> + '_ {
        self.legacy_usages.iter()
    }

    pub fn emit_metrics(&self, otel: &OtelManager) {
        for feature in FEATURES {
            if self.enabled(feature.id) != feature.default_enabled {
                otel.counter(
                    "codex.feature.state",
                    1,
                    &[
                        ("feature", feature.key),
                        ("value", &self.enabled(feature.id).to_string()),
                    ],
                );
            }
        }
    }

    /// Apply a table of key -> bool toggles (e.g. from TOML).
    pub fn apply_map(&mut self, m: &BTreeMap<String, bool>) {
        for (k, v) in m {
            match k.as_str() {
                "web_search_request" => {
                    self.record_legacy_usage_force(
                        "features.web_search_request",
                        Feature::WebSearchRequest,
                    );
                }
                "web_search_cached" => {
                    self.record_legacy_usage_force(
                        "features.web_search_cached",
                        Feature::WebSearchCached,
                    );
                }
                _ => {}
            }
            match feature_for_key(k) {
                Some(feat) => {
                    if k != feat.key() {
                        self.record_legacy_usage(k.as_str(), feat);
                    }
                    if *v {
                        self.enable(feat);
                    } else {
                        self.disable(feat);
                    }
                }
                None => {
                    tracing::warn!("unknown feature key in config: {k}");
                }
            }
        }
    }

    pub fn from_config(
        cfg: &ConfigToml,
        config_profile: &ConfigProfile,
        overrides: FeatureOverrides,
    ) -> Self {
        let mut features = Features::with_defaults();

        let base_legacy = LegacyFeatureToggles {
            experimental_use_freeform_apply_patch: cfg.experimental_use_freeform_apply_patch,
            experimental_use_unified_exec_tool: cfg.experimental_use_unified_exec_tool,
            tools_web_search: cfg.tools.as_ref().and_then(|t| t.web_search),
            ..Default::default()
        };
        base_legacy.apply(&mut features);

        if let Some(base_features) = cfg.features.as_ref() {
            features.apply_map(&base_features.entries);
        }

        let profile_legacy = LegacyFeatureToggles {
            include_apply_patch_tool: config_profile.include_apply_patch_tool,
            experimental_use_freeform_apply_patch: config_profile
                .experimental_use_freeform_apply_patch,

            experimental_use_unified_exec_tool: config_profile.experimental_use_unified_exec_tool,
            tools_web_search: config_profile.tools_web_search,
        };
        profile_legacy.apply(&mut features);
        if let Some(profile_features) = config_profile.features.as_ref() {
            features.apply_map(&profile_features.entries);
        }

        overrides.apply(&mut features);
        disable_removed_features(&mut features);
        enable_always_on_features(&mut features);

        features
    }

    pub fn enabled_features(&self) -> Vec<Feature> {
        self.enabled.iter().copied().collect()
    }
}

fn disable_removed_features(features: &mut Features) {
    features.disable(Feature::Apps);
}

fn enable_always_on_features(features: &mut Features) {
    features.enable(Feature::AgentJobs);
    features.enable(Feature::BackfillCompactPlanContext);
}

fn legacy_usage_notice(alias: &str, feature: Feature) -> (String, Option<String>) {
    let canonical = feature.key();
    match feature {
        Feature::WebSearchRequest | Feature::WebSearchCached => {
            let label = match alias {
                "web_search" => "[features].web_search",
                "tools.web_search" => "[tools].web_search",
                "features.web_search_request" | "web_search_request" => {
                    "[features].web_search_request"
                }
                "features.web_search_cached" | "web_search_cached" => {
                    "[features].web_search_cached"
                }
                _ => alias,
            };
            let summary = format!("`{label}` is deprecated. Use `web_search` instead.");
            (summary, Some(web_search_details().to_string()))
        }
        _ => {
            let summary = format!("`{alias}` is deprecated. Use `[features].{canonical}` instead.");
            let details = if alias == canonical {
                None
            } else {
                Some(format!(
                    "Enable it with `--enable {canonical}` or `[features].{canonical}` in config.toml. See https://github.com/openai/codex/blob/main/docs/config.md#feature-flags for details."
                ))
            };
            (summary, details)
        }
    }
}

fn web_search_details() -> &'static str {
    "Set `web_search` to `\"live\"`, `\"cached\"`, or `\"disabled\"` in config.toml."
}

/// Keys accepted in `[features]` tables.
fn feature_for_key(key: &str) -> Option<Feature> {
    for spec in FEATURES {
        if spec.key == key {
            return Some(spec.id);
        }
    }
    legacy::feature_for_key(key)
}

/// Returns `true` if the provided string matches a known feature toggle key.
pub fn is_known_feature_key(key: &str) -> bool {
    feature_for_key(key).is_some()
}

/// Deserializable features table for TOML.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, JsonSchema)]
pub struct FeaturesToml {
    #[serde(flatten)]
    pub entries: BTreeMap<String, bool>,
}

/// Single, easy-to-read registry of all feature definitions.
#[derive(Debug, Clone, Copy)]
pub struct FeatureSpec {
    pub id: Feature,
    pub key: &'static str,
    pub stage: Stage,
    pub default_enabled: bool,
}

pub const FEATURES: &[FeatureSpec] = &[
    // Stable features.
    FeatureSpec {
        id: Feature::GhostCommit,
        key: "undo",
        stage: Stage::Stable,
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::ShellTool,
        key: "shell_tool",
        stage: Stage::Stable,
        default_enabled: true,
    },
    FeatureSpec {
        id: Feature::Goals,
        key: "goals",
        stage: Stage::Stable,
        default_enabled: true,
    },
    FeatureSpec {
        id: Feature::MemoryTool,
        key: "memories",
        stage: Stage::Experimental {
            name: "Memories",
            menu_description: "Allow LHA to create new memories from conversations and bring relevant memories into new conversations.",
            announcement: "NEW: LHA can now generate and use memories. Try it now with `/memories`.",
        },
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::UnifiedExec,
        key: "unified_exec",
        stage: Stage::Stable,
        default_enabled: !cfg!(windows),
    },
    FeatureSpec {
        id: Feature::WebSearchRequest,
        key: "web_search_request",
        stage: Stage::Deprecated,
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::WebSearchCached,
        key: "web_search_cached",
        stage: Stage::Deprecated,
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::ShellSnapshot,
        key: "shell_snapshot",
        stage: Stage::Experimental {
            name: "Shell snapshot",
            menu_description: "Snapshot your shell environment to avoid re-running login scripts for every command.",
            announcement: "NEW! Try shell snapshotting to make your LHA faster. Enable in /experimental!",
        },
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::RuntimeMetrics,
        key: "runtime_metrics",
        stage: Stage::UnderDevelopment,
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::Sqlite,
        key: "sqlite",
        stage: Stage::UnderDevelopment,
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::ChildAgentsMd,
        key: "child_agents_md",
        stage: Stage::UnderDevelopment,
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::ApplyPatchFreeform,
        key: "apply_patch_freeform",
        stage: Stage::UnderDevelopment,
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::ExecPolicy,
        key: "exec_policy",
        stage: Stage::UnderDevelopment,
        default_enabled: true,
    },
    FeatureSpec {
        id: Feature::RequestRule,
        key: "request_rule",
        stage: Stage::Stable,
        default_enabled: true,
    },
    FeatureSpec {
        id: Feature::WindowsSandbox,
        key: "experimental_windows_sandbox",
        stage: Stage::UnderDevelopment,
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::WindowsSandboxElevated,
        key: "elevated_windows_sandbox",
        stage: Stage::UnderDevelopment,
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::RemoteCompaction,
        key: "remote_compaction",
        stage: Stage::UnderDevelopment,
        default_enabled: true,
    },
    FeatureSpec {
        id: Feature::InputSlimming,
        key: "input_slimming",
        stage: Stage::Experimental {
            name: "Input slimming",
            menu_description: "Slim large tool results in model requests to reduce token usage while keeping originals retrievable.",
            announcement: "NEW: Input slimming can reduce model input size for large tool outputs. Enable in /experimental.",
        },
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::RemoteModels,
        key: "remote_models",
        stage: Stage::UnderDevelopment,
        default_enabled: true,
    },
    FeatureSpec {
        id: Feature::PowershellUtf8,
        key: "powershell_utf8",
        #[cfg(windows)]
        stage: Stage::Stable,
        #[cfg(windows)]
        default_enabled: true,
        #[cfg(not(windows))]
        stage: Stage::UnderDevelopment,
        #[cfg(not(windows))]
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::EnableRequestCompression,
        key: "enable_request_compression",
        stage: Stage::Stable,
        default_enabled: true,
    },
    FeatureSpec {
        id: Feature::ForceHttp1Streaming,
        key: "force_http1_streaming",
        stage: Stage::Experimental {
            name: "Force HTTP/1.1 streaming",
            menu_description: "Use HTTP/1.1 for model streaming requests to avoid proxy or gateway issues with HTTP/2.",
            announcement: "NEW: Force HTTP/1.1 for model streaming requests in /experimental.",
        },
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::AgentJobs,
        key: "agent_jobs",
        stage: Stage::Stable,
        default_enabled: true,
    },
    FeatureSpec {
        id: Feature::Apps,
        key: "apps",
        stage: Stage::Removed,
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::SkillMcpDependencyInstall,
        key: "skill_mcp_dependency_install",
        stage: Stage::Stable,
        default_enabled: true,
    },
    FeatureSpec {
        id: Feature::SkillEnvVarDependencyPrompt,
        key: "skill_env_var_dependency_prompt",
        stage: Stage::UnderDevelopment,
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::Steer,
        key: "steer",
        stage: Stage::Experimental {
            name: "Steer conversation",
            menu_description: "Enter submits immediately; Tab queues messages when a task is running.",
            announcement: "NEW! Try Steer mode: Enter submits immediately, Tab queues. Enable in /experimental!",
        },
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::BackfillCompactPlanContext,
        key: "backfill_compact_plan_context",
        stage: Stage::Stable,
        default_enabled: true,
    },
    FeatureSpec {
        id: Feature::Identities,
        key: "identities",
        stage: Stage::Stable,
        default_enabled: true,
    },
    FeatureSpec {
        id: Feature::Personality,
        key: "personality",
        stage: Stage::Stable,
        default_enabled: true,
    },
    FeatureSpec {
        id: Feature::PreventIdleSleep,
        key: "prevent_idle_sleep",
        stage: if cfg!(any(
            target_os = "macos",
            target_os = "linux",
            target_os = "windows"
        )) {
            Stage::Experimental {
                name: "Prevent sleep while running",
                menu_description: "Keep your computer awake while LHA is running a thread.",
                announcement: "NEW: Prevent sleep while running is now available in /experimental.",
            }
        } else {
            Stage::UnderDevelopment
        },
        default_enabled: false,
    },
    FeatureSpec {
        id: Feature::ResponsesWebsockets,
        key: "responses_websockets",
        stage: Stage::UnderDevelopment,
        default_enabled: false,
    },
];

/// Push a warning event if any under-development features are enabled.
pub fn maybe_push_unstable_features_warning(
    config: &Config,
    post_session_configured_events: &mut Vec<Event>,
) {
    if config.suppress_unstable_features_warning {
        return;
    }

    let mut under_development_feature_keys = Vec::new();
    if let Some(table) = config
        .config_layer_stack
        .effective_config()
        .get("features")
        .and_then(TomlValue::as_table)
    {
        for (key, value) in table {
            if value.as_bool() != Some(true) {
                continue;
            }
            let Some(spec) = FEATURES.iter().find(|spec| spec.key == key.as_str()) else {
                continue;
            };
            if !config.features.enabled(spec.id) {
                continue;
            }
            if matches!(spec.stage, Stage::UnderDevelopment) {
                under_development_feature_keys.push(spec.key.to_string());
            }
        }
    }

    if under_development_feature_keys.is_empty() {
        return;
    }

    let under_development_feature_keys = under_development_feature_keys.join(", ");
    let config_path = config.lha_home.join(CONFIG_TOML_FILE).display().to_string();
    let message = format!(
        "Under-development features enabled: {under_development_feature_keys}. Under-development features are incomplete and may behave unpredictably. To suppress this warning, set `suppress_unstable_features_warning = true` in {config_path}."
    );
    post_session_configured_events.push(Event {
        id: "".to_owned(),
        msg: EventMsg::Warning(WarningEvent { message }),
    });
}

#[cfg(test)]
mod tests {
    use super::Feature;
    use super::FeatureOverrides;
    use super::Features;
    use super::FeaturesToml;
    use super::Stage;
    use crate::product::agent::config::ConfigToml;
    use crate::product::agent::config::profile::ConfigProfile;
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;

    #[test]
    fn unified_exec_is_stable_and_enabled_by_default_off_windows() {
        let stage = Feature::UnifiedExec.stage();

        assert_eq!(Stage::Stable, stage);
        assert_eq!(!cfg!(windows), Feature::UnifiedExec.default_enabled());
        assert_eq!(None, stage.experimental_menu_name());
        assert_eq!(None, stage.experimental_menu_description());
        assert_eq!(None, stage.experimental_announcement());
    }

    #[test]
    fn agent_jobs_is_stable_and_enabled_by_default() {
        let stage = Feature::AgentJobs.stage();

        assert_eq!(Stage::Stable, stage);
        assert_eq!(true, Feature::AgentJobs.default_enabled());
        assert_eq!(None, stage.experimental_menu_name());
        assert_eq!(None, stage.experimental_menu_description());
        assert_eq!(None, stage.experimental_announcement());
    }

    #[test]
    fn agent_jobs_stays_enabled_when_base_config_sets_false() {
        let cfg = ConfigToml {
            features: Some(FeaturesToml {
                entries: BTreeMap::from([("agent_jobs".to_string(), false)]),
            }),
            ..Default::default()
        };

        let features =
            Features::from_config(&cfg, &ConfigProfile::default(), FeatureOverrides::default());

        assert!(features.enabled(Feature::AgentJobs));
    }

    #[test]
    fn agent_jobs_stays_enabled_when_profile_sets_false() {
        let profile = ConfigProfile {
            features: Some(FeaturesToml {
                entries: BTreeMap::from([("agent_jobs".to_string(), false)]),
            }),
            ..Default::default()
        };

        let features = Features::from_config(
            &ConfigToml::default(),
            &profile,
            FeatureOverrides::default(),
        );

        assert!(features.enabled(Feature::AgentJobs));
    }

    #[test]
    fn backfill_compact_plan_context_stays_enabled_when_base_config_sets_false() {
        let cfg = ConfigToml {
            features: Some(FeaturesToml {
                entries: BTreeMap::from([("backfill_compact_plan_context".to_string(), false)]),
            }),
            ..Default::default()
        };

        let features =
            Features::from_config(&cfg, &ConfigProfile::default(), FeatureOverrides::default());

        assert!(features.enabled(Feature::BackfillCompactPlanContext));
    }

    #[test]
    fn prevent_idle_sleep_has_expected_stage_and_metadata() {
        let stage = Feature::PreventIdleSleep.stage();

        assert_eq!(false, Feature::PreventIdleSleep.default_enabled());
        if cfg!(any(
            target_os = "macos",
            target_os = "linux",
            target_os = "windows"
        )) {
            assert_eq!(
                Some("Prevent sleep while running"),
                stage.experimental_menu_name()
            );
            assert_eq!(
                Some("Keep your computer awake while LHA is running a thread."),
                stage.experimental_menu_description()
            );
            assert_eq!(
                Some("NEW: Prevent sleep while running is now available in /experimental."),
                stage.experimental_announcement()
            );
        } else {
            assert_eq!(Stage::UnderDevelopment, stage);
            assert_eq!(None, stage.experimental_menu_name());
            assert_eq!(None, stage.experimental_menu_description());
            assert_eq!(None, stage.experimental_announcement());
        }
    }

    #[test]
    fn backfill_compact_plan_context_is_stable_and_enabled_by_default() {
        let stage = Feature::BackfillCompactPlanContext.stage();

        assert_eq!(Stage::Stable, stage);
        assert_eq!(true, Feature::BackfillCompactPlanContext.default_enabled());
        assert_eq!(None, stage.experimental_menu_name());
        assert_eq!(None, stage.experimental_menu_description());
        assert_eq!(None, stage.experimental_announcement());
    }

    #[test]
    fn force_http1_streaming_is_experimental_and_disabled_by_default() {
        let stage = Feature::ForceHttp1Streaming.stage();

        assert_eq!(false, Feature::ForceHttp1Streaming.default_enabled());
        assert_eq!(
            Some("Force HTTP/1.1 streaming"),
            stage.experimental_menu_name()
        );
        assert_eq!(
            Some(
                "Use HTTP/1.1 for model streaming requests to avoid proxy or gateway issues with HTTP/2.",
            ),
            stage.experimental_menu_description()
        );
        assert_eq!(
            Some("NEW: Force HTTP/1.1 for model streaming requests in /experimental."),
            stage.experimental_announcement()
        );
    }

    #[test]
    fn force_http1_streaming_can_be_enabled_from_config() {
        let cfg = ConfigToml {
            features: Some(FeaturesToml {
                entries: BTreeMap::from([("force_http1_streaming".to_string(), true)]),
            }),
            ..Default::default()
        };

        let features =
            Features::from_config(&cfg, &ConfigProfile::default(), FeatureOverrides::default());

        assert!(features.enabled(Feature::ForceHttp1Streaming));
    }

    #[test]
    fn input_slimming_feature_defaults_off() {
        let stage = Feature::InputSlimming.stage();

        assert_eq!(
            Stage::Experimental {
                name: "Input slimming",
                menu_description: "Slim large tool results in model requests to reduce token usage while keeping originals retrievable.",
                announcement: "NEW: Input slimming can reduce model input size for large tool outputs. Enable in /experimental.",
            },
            stage
        );
        assert!(!Feature::InputSlimming.default_enabled());
        assert_eq!(Some("Input slimming"), stage.experimental_menu_name());
        assert_eq!(
            Some(
                "Slim large tool results in model requests to reduce token usage while keeping originals retrievable.",
            ),
            stage.experimental_menu_description()
        );
        assert_eq!(
            Some(
                "NEW: Input slimming can reduce model input size for large tool outputs. Enable in /experimental.",
            ),
            stage.experimental_announcement()
        );
        assert!(!Features::with_defaults().enabled(Feature::InputSlimming));
    }

    #[test]
    fn features_table_accepts_input_slimming() {
        let cfg = ConfigToml {
            features: Some(FeaturesToml {
                entries: BTreeMap::from([("input_slimming".to_string(), true)]),
            }),
            ..Default::default()
        };

        let features =
            Features::from_config(&cfg, &ConfigProfile::default(), FeatureOverrides::default());

        assert!(features.enabled(Feature::InputSlimming));
    }

    #[test]
    fn generated_config_schema_contains_input_slimming() {
        let schema = include_str!("../config.schema.json");

        assert!(schema.contains("\"input_slimming\""));
    }

    #[test]
    fn apps_is_removed_and_hidden_from_experimental_ui() {
        let stage = Feature::Apps.stage();

        assert_eq!(Stage::Removed, stage);
        assert_eq!(false, Feature::Apps.default_enabled());
        assert_eq!(None, stage.experimental_menu_name());
        assert_eq!(None, stage.experimental_menu_description());
        assert_eq!(None, stage.experimental_announcement());
    }
}
