use super::cache::ModelsCacheManager;
use crate::auth::AuthManager;
use crate::auth::AuthMode;
use crate::config::Config;
use crate::config::ConfigToml;
use crate::config::display_model_provider_ref;
use crate::config::generated_provider_profile_name;
use crate::config::model_provider_cache_key;
use crate::config::model_provider_id_from_ref;
use crate::config::model_ref::ModelRef;
use crate::config::models_json::ModelsJson;
use crate::default_client::build_reqwest_client;
use crate::error::CodexErr;
use crate::error::Result as CoreResult;
use crate::features::Feature;
use crate::models_manager::collaboration_mode_presets::builtin_collaboration_mode_presets;
use crate::models_manager::model_info;
use crate::models_manager::model_presets::builtin_model_presets;
use crate::runtime_builder::auth_context_from_auth;
pub use adam_llm::CatalogRefreshStrategy as RefreshStrategy;
use adam_llm::RuntimeEndpoint;
use adam_llm::fetch_remote_models;
use adam_protocol::config_types::CollaborationModeMask;
use adam_protocol::openai_models::ModelInfo;
use adam_protocol::openai_models::ModelPreset;
use adam_protocol::openai_models::ModelsResponse;
use adam_protocol::openai_models::ReasoningEffort;
use http::HeaderMap;
use once_cell::sync::Lazy;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::sync::TryLockError;
use tracing::error;

const MODEL_CACHE_FILE: &str = "models_cache.json";
const DEFAULT_MODEL_CACHE_TTL: Duration = Duration::from_secs(300);
const MODELS_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);
const OPENAI_PROVIDER_ID: &str = "openai";
const OFFICIAL_OPENAI_PROVIDER_DESCRIPTION: &str = "Official model from OpenAI provider.";
static BUNDLED_REMOTE_MODEL_SLUGS: Lazy<HashSet<String>> = Lazy::new(|| {
    ModelsManager::load_remote_models_from_file()
        .unwrap_or_default()
        .into_iter()
        .map(|model| model.slug)
        .collect()
});

/// Coordinates remote model discovery plus cached metadata on disk.
#[derive(Debug)]
pub struct ModelsManager {
    adam_home: PathBuf,
    local_models: Vec<ModelPreset>,
    remote_models: RwLock<Vec<ModelInfo>>,
    auth_manager: Arc<AuthManager>,
    etag: RwLock<Option<String>>,
    cache_manager: StdRwLock<ModelsCacheManager>,
    model_provider_id: StdRwLock<String>,
    provider: StdRwLock<RuntimeEndpoint>,
}

impl ModelsManager {
    /// Construct a manager scoped to the provided `AuthManager` and model provider.
    ///
    /// Uses `adam_home` to store provider-scoped cached model metadata and initializes with
    /// built-in presets.
    pub fn new(
        adam_home: PathBuf,
        auth_manager: Arc<AuthManager>,
        model_provider_id: &str,
        provider: RuntimeEndpoint,
    ) -> Self {
        let cache_path = models_cache_path(&adam_home, model_provider_id);
        let cache_manager = ModelsCacheManager::new(cache_path, DEFAULT_MODEL_CACHE_TTL);
        let remote_models = if Self::provider_uses_model_catalog(&provider) {
            Self::load_remote_models_from_file().unwrap_or_default()
        } else {
            Vec::new()
        };

        Self {
            adam_home,
            local_models: builtin_model_presets(auth_manager.get_internal_auth_mode()),
            remote_models: RwLock::new(remote_models),
            auth_manager,
            etag: RwLock::new(None),
            cache_manager: StdRwLock::new(cache_manager),
            model_provider_id: StdRwLock::new(model_provider_id.to_string()),
            provider: StdRwLock::new(provider),
        }
    }

    pub fn set_provider(&self, provider: RuntimeEndpoint) {
        *self
            .provider
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = provider;
    }

    pub async fn switch_provider(&self, model_provider_id: &str, provider: RuntimeEndpoint) {
        let uses_model_catalog = Self::provider_uses_model_catalog(&provider);
        *self
            .cache_manager
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = ModelsCacheManager::new(
            models_cache_path(&self.adam_home, model_provider_id),
            DEFAULT_MODEL_CACHE_TTL,
        );
        *self
            .model_provider_id
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = model_provider_id.to_string();
        *self
            .provider
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = provider;
        *self.remote_models.write().await = if uses_model_catalog {
            Self::load_remote_models_from_file().unwrap_or_default()
        } else {
            Vec::new()
        };
        *self.etag.write().await = None;
        if uses_model_catalog {
            self.try_load_cache().await;
        }
    }

    /// List all available models, refreshing according to the specified strategy.
    ///
    /// Returns model presets sorted by priority and filtered by auth mode and visibility.
    pub async fn list_models(
        &self,
        config: &Config,
        refresh_strategy: RefreshStrategy,
    ) -> Vec<ModelPreset> {
        if let Err(err) = self
            .refresh_available_models(config, refresh_strategy)
            .await
        {
            error!("failed to refresh available models: {err}");
        }
        let remote_models = self.get_remote_models(config).await;
        self.build_available_models(remote_models)
    }

    /// List the models that should be shown in picker UIs.
    ///
    /// Includes the current configured model when it is not otherwise visible in
    /// the picker. Without any configured auth, the picker collapses down to the
    /// configured model when one is present.
    pub async fn list_picker_models(
        &self,
        config: &Config,
        refresh_strategy: RefreshStrategy,
    ) -> Vec<ModelPreset> {
        let available_models = self.list_models(config, refresh_strategy).await;
        self.build_picker_models(config, available_models)
    }

    /// List the models that should be shown by the `/model` command.
    ///
    /// Without ChatGPT auth, this is limited to models explicitly declared in
    /// `config.toml`. With ChatGPT auth, `/model` always includes the official
    /// OpenAI switcher models so users can switch back to them from any active
    /// provider, even when `remote_models = false`, and any additional
    /// configured models are appended.
    pub async fn list_model_switcher_models(
        &self,
        config: &Config,
        refresh_strategy: RefreshStrategy,
    ) -> Vec<ModelPreset> {
        let available_models = self.list_models(config, refresh_strategy).await;
        self.build_model_switcher_models(config, available_models)
    }

    /// List collaboration mode presets.
    ///
    /// Returns a static set of presets seeded with the configured model.
    pub fn list_collaboration_modes(&self) -> Vec<CollaborationModeMask> {
        builtin_collaboration_mode_presets()
    }

    /// Attempt to list models without blocking, using the current cached state.
    ///
    /// Returns an error if the internal lock cannot be acquired.
    pub fn try_list_models(&self, config: &Config) -> Result<Vec<ModelPreset>, TryLockError> {
        let remote_models = self.try_get_remote_models(config)?;
        Ok(self.build_available_models(remote_models))
    }

    /// Attempt to list picker-visible models without blocking, using cached state.
    pub fn try_list_picker_models(
        &self,
        config: &Config,
    ) -> Result<Vec<ModelPreset>, TryLockError> {
        let available_models = self.try_list_models(config)?;
        Ok(self.build_picker_models(config, available_models))
    }

    /// Attempt to list `/model` command models without blocking, using cached state.
    pub fn try_list_model_switcher_models(
        &self,
        config: &Config,
    ) -> Result<Vec<ModelPreset>, TryLockError> {
        let available_models = self.try_list_models(config)?;
        Ok(self.build_model_switcher_models(config, available_models))
    }

    /// Attempt to determine whether the selected model is an official OpenAI
    /// `/model` switcher entry without blocking on model refresh.
    pub fn try_is_official_openai_model(
        &self,
        config: &Config,
        model: &str,
        model_provider_id: &str,
    ) -> Result<bool, TryLockError> {
        if model_provider_id != OPENAI_PROVIDER_ID {
            return Ok(false);
        }

        let available_models = self.try_list_models(config)?;
        Ok(self
            .official_openai_switcher_model(model, Some(model_provider_id), &available_models)
            .is_some())
    }

    // todo(aibrahim): should be visible to core only and sent on session_configured event
    /// Get the model identifier to use, refreshing according to the specified strategy.
    ///
    /// If `model` is provided, returns it directly. Otherwise selects the default based on
    /// auth mode and available models.
    pub async fn get_default_model(
        &self,
        model: &Option<String>,
        config: &Config,
        refresh_strategy: RefreshStrategy,
    ) -> CoreResult<String> {
        if let Some(model) = model.as_ref() {
            return Ok(model.to_string());
        }
        if self.provider_snapshot().requires_explicit_model_selection() {
            return Err(CodexErr::Fatal(
                "dialect = \"messages\" requires an explicit model".to_string(),
            ));
        }
        if let Err(err) = self
            .refresh_available_models(config, refresh_strategy)
            .await
        {
            error!("failed to refresh available models: {err}");
        }
        let remote_models = self.get_remote_models(config).await;
        let available = self.build_available_models(remote_models);
        Ok(available
            .iter()
            .find(|model| model.is_default)
            .or_else(|| available.first())
            .map(|model| model.model.clone())
            .unwrap_or_default())
    }

    // todo(aibrahim): look if we can tighten it to pub(crate)
    /// Look up model metadata, applying remote overrides and config adjustments.
    ///
    /// Bundled/remote models are the first source of truth for online model metadata. The
    /// handwritten `model_info.rs` entries remain as the offline fallback for slugs that are not
    /// present in bundled/remote metadata.
    pub async fn get_model_info(&self, model: &str, config: &Config) -> ModelInfo {
        let remote = self
            .get_remote_models(config)
            .await
            .into_iter()
            .find(|m| m.slug == model);
        let model = if let Some(remote) = remote {
            remote
        } else {
            model_info::find_model_info_for_slug(model)
        };
        model_info::with_config_overrides(model, config)
    }

    /// Refresh models if the provided ETag differs from the cached ETag.
    ///
    /// Uses `Online` strategy to fetch latest models when ETags differ.
    pub(crate) async fn refresh_if_new_etag(&self, etag: String, config: &Config) {
        let current_etag = self.get_etag().await;
        if current_etag.clone().is_some() && current_etag.as_deref() == Some(etag.as_str()) {
            if let Err(err) = self.cache_manager_snapshot().renew_cache_ttl().await {
                error!("failed to renew cache TTL: {err}");
            }
            return;
        }
        if let Err(err) = self
            .refresh_available_models(config, RefreshStrategy::Online)
            .await
        {
            error!("failed to refresh available models: {err}");
        }
    }

    /// Refresh available models according to the specified strategy.
    async fn refresh_available_models(
        &self,
        config: &Config,
        refresh_strategy: RefreshStrategy,
    ) -> CoreResult<()> {
        if !Self::provider_uses_model_catalog(&self.provider_snapshot()) {
            self.clear_remote_model_state().await;
            return Ok(());
        }
        if !config.features.enabled(Feature::RemoteModels)
            || self.auth_manager.get_internal_auth_mode() == Some(AuthMode::ApiKey)
        {
            return Ok(());
        }

        match refresh_strategy {
            RefreshStrategy::Offline => {
                // Only try to load from cache, never fetch
                self.try_load_cache().await;
                Ok(())
            }
            RefreshStrategy::OnlineIfUncached => {
                // Try cache first, fall back to online if unavailable
                if self.try_load_cache().await {
                    return Ok(());
                }
                self.fetch_and_update_models().await
            }
            RefreshStrategy::Online => {
                // Always fetch from network
                self.fetch_and_update_models().await
            }
        }
    }

    async fn fetch_and_update_models(&self) -> CoreResult<()> {
        let _timer =
            adam_otel::start_global_timer("codex.remote_models.fetch_update.duration_ms", &[]);
        if !Self::provider_uses_model_catalog(&self.provider_snapshot()) {
            self.clear_remote_model_state().await;
            return Ok(());
        }
        let auth = self.auth_manager.auth().await;
        let provider = self.provider_snapshot();
        let auth_context = auth_context_from_auth(auth.clone(), &provider)?;

        let client_version = format_client_version_to_whole();
        let (models, etag) = fetch_remote_models(
            build_reqwest_client(),
            &provider,
            auth_context,
            &client_version,
            HeaderMap::new(),
            MODELS_REFRESH_TIMEOUT,
        )
        .await?;

        let models: Vec<ModelInfo> = models.into_iter().map(Into::into).collect();

        self.apply_remote_models(models.clone()).await;
        *self.etag.write().await = etag.clone();
        self.cache_manager_snapshot()
            .persist_cache(&models, etag)
            .await;
        Ok(())
    }

    async fn get_etag(&self) -> Option<String> {
        self.etag.read().await.clone()
    }

    /// Replace the cached remote models and rebuild the derived presets list.
    async fn apply_remote_models(&self, models: Vec<ModelInfo>) {
        let mut existing_models = Self::load_remote_models_from_file().unwrap_or_default();
        for model in models {
            if let Some(existing_index) = existing_models
                .iter()
                .position(|existing| existing.slug == model.slug)
            {
                existing_models[existing_index] = model;
            } else {
                existing_models.push(model);
            }
        }
        *self.remote_models.write().await = existing_models;
    }

    fn load_remote_models_from_file() -> Result<Vec<ModelInfo>, std::io::Error> {
        let file_contents = include_str!("../../models.json");
        let response: ModelsResponse = serde_json::from_str(file_contents)?;
        Ok(response.models)
    }

    /// Attempt to satisfy the refresh from the cache when it matches the provider and TTL.
    async fn try_load_cache(&self) -> bool {
        let _timer =
            adam_otel::start_global_timer("codex.remote_models.load_cache.duration_ms", &[]);
        if !Self::provider_uses_model_catalog(&self.provider_snapshot()) {
            self.clear_remote_model_state().await;
            return false;
        }
        let cache = match self.cache_manager_snapshot().load_fresh().await {
            Some(cache) => cache,
            None => return false,
        };
        let models = cache.models.clone();
        *self.etag.write().await = cache.etag.clone();
        self.apply_remote_models(models).await;
        true
    }

    /// Merge remote model metadata into picker-ready presets, preserving existing entries.
    fn build_available_models(&self, mut remote_models: Vec<ModelInfo>) -> Vec<ModelPreset> {
        remote_models.sort_by(|a, b| a.priority.cmp(&b.priority));

        let remote_presets: Vec<ModelPreset> = remote_models
            .into_iter()
            .map(Into::into)
            .map(|preset| self.assign_available_model_provider_identity(preset))
            .collect();
        let existing_presets = self.builtin_presets_for_provider();
        let existing_presets = existing_presets
            .into_iter()
            .map(|preset| self.assign_builtin_model_provider_identity(preset))
            .collect();
        let mut merged_presets = ModelPreset::merge(remote_presets, existing_presets);
        let chatgpt_mode = matches!(
            self.auth_manager.get_internal_auth_mode(),
            Some(AuthMode::Chatgpt)
        );
        merged_presets = ModelPreset::filter_by_auth(merged_presets, chatgpt_mode);

        for preset in &mut merged_presets {
            preset.is_default = false;
        }
        if let Some(default) = merged_presets
            .iter_mut()
            .find(|preset| preset.show_in_picker)
        {
            default.is_default = true;
        } else if let Some(default) = merged_presets.first_mut() {
            default.is_default = true;
        }

        merged_presets
    }

    fn build_picker_models(
        &self,
        config: &Config,
        available_models: Vec<ModelPreset>,
    ) -> Vec<ModelPreset> {
        let has_auth = self.auth_manager.get_internal_auth_mode().is_some()
            || self.provider_snapshot().has_local_auth();
        let mut picker_models: Vec<ModelPreset> = available_models
            .iter()
            .filter(|preset| preset.show_in_picker)
            .cloned()
            .collect();
        self.apply_openai_official_switcher_metadata(&mut picker_models);

        let custom_model = self.configured_picker_model(config, &picker_models, &available_models);

        match (has_auth, custom_model) {
            (true, Some(custom_model)) => {
                picker_models.push(custom_model);
                picker_models
            }
            (true, None) => picker_models,
            (false, Some(mut custom_model)) => {
                custom_model.is_default = true;
                vec![custom_model]
            }
            (false, None) => picker_models,
        }
    }

    fn picker_contains_model_identity(
        picker_models: &[ModelPreset],
        candidate: &ModelPreset,
    ) -> bool {
        picker_models
            .iter()
            .any(|preset| Self::same_model_identity(preset, candidate))
    }

    fn build_model_switcher_models(
        &self,
        config: &Config,
        available_models: Vec<ModelPreset>,
    ) -> Vec<ModelPreset> {
        let chatgpt_auth = matches!(
            self.auth_manager.get_internal_auth_mode(),
            Some(AuthMode::Chatgpt)
        );
        let configured_models = if chatgpt_auth {
            self.configured_chatgpt_model_switcher_models(config, &available_models)
        } else {
            self.configured_model_switcher_models(config, &available_models)
        };
        if !chatgpt_auth {
            return configured_models;
        }

        let mut picker_models = self.official_openai_switcher_models(&available_models);
        for picker_model in self.build_picker_models(config, available_models) {
            if Self::picker_contains_model_identity(&picker_models, &picker_model) {
                continue;
            }
            picker_models.push(picker_model);
        }
        self.apply_openai_official_switcher_metadata(&mut picker_models);
        for mut configured_model in configured_models {
            if picker_models
                .iter()
                .any(|picker_model| Self::same_model_identity(picker_model, &configured_model))
            {
                continue;
            }
            configured_model.is_default = false;
            picker_models.push(configured_model);
        }
        picker_models
    }

    fn official_openai_switcher_models(
        &self,
        available_models: &[ModelPreset],
    ) -> Vec<ModelPreset> {
        let available_official_models = available_models
            .iter()
            .filter(|preset| preset.model_provider_id.as_deref() == Some(OPENAI_PROVIDER_ID))
            .cloned()
            .collect();
        // `/model` intentionally keeps the bundled official OpenAI catalog
        // available for ChatGPT-authenticated users, even when remote models
        // are filtered out of the general picker/default-model paths.
        let mut picker_models = ModelPreset::merge(
            available_official_models,
            self.bundled_openai_available_models(),
        );
        self.apply_openai_official_switcher_metadata(&mut picker_models);
        picker_models
            .into_iter()
            .filter(|preset| preset.show_in_picker)
            .collect()
    }

    fn bundled_openai_available_models(&self) -> Vec<ModelPreset> {
        let mut remote_models = Self::load_remote_models_from_file().unwrap_or_default();
        remote_models.sort_by(|a, b| a.priority.cmp(&b.priority));

        let remote_presets = remote_models
            .into_iter()
            .map(Into::into)
            .map(|preset| self.assign_builtin_model_provider_identity(preset))
            .collect();
        let existing_presets = self
            .local_models
            .clone()
            .into_iter()
            .map(|preset| self.assign_builtin_model_provider_identity(preset))
            .collect();
        let mut merged_presets = ModelPreset::merge(remote_presets, existing_presets);

        for preset in &mut merged_presets {
            preset.is_default = false;
        }
        if let Some(default) = merged_presets
            .iter_mut()
            .find(|preset| preset.show_in_picker)
        {
            default.is_default = true;
        } else if let Some(default) = merged_presets.first_mut() {
            default.is_default = true;
        }

        merged_presets
    }

    fn configured_chatgpt_model_switcher_models(
        &self,
        config: &Config,
        available_models: &[ModelPreset],
    ) -> Vec<ModelPreset> {
        let Some(config_toml) = self.config_toml(config) else {
            return Vec::new();
        };

        let mut presets: Vec<ModelPreset> = self
            .configured_model_entries(&config_toml, config)
            .into_iter()
            .map(|(model, provider_id)| {
                self.configured_model_switcher_preset_from_config_toml(
                    &model,
                    provider_id.as_deref(),
                    &config_toml,
                    config,
                    available_models,
                )
            })
            .collect();

        for preset in &mut presets {
            preset.is_default = false;
        }
        if let Some(default) = presets.first_mut() {
            default.is_default = true;
        }

        presets
    }

    fn configured_model_switcher_models(
        &self,
        config: &Config,
        available_models: &[ModelPreset],
    ) -> Vec<ModelPreset> {
        let Some(config_toml) = self.config_toml(config) else {
            return Vec::new();
        };

        let mut presets: Vec<ModelPreset> = self
            .configured_model_entries(&config_toml, config)
            .into_iter()
            .map(|(model, provider_id)| {
                self.configured_model_preset_from_config_toml(
                    &model,
                    provider_id.as_deref(),
                    &config_toml,
                    config,
                    available_models,
                )
            })
            .collect();

        for preset in &mut presets {
            preset.is_default = false;
        }
        if let Some(default) = presets.first_mut() {
            default.is_default = true;
        }

        presets
    }

    fn configured_model_switcher_preset_from_config_toml(
        &self,
        model: &str,
        model_provider_id: Option<&str>,
        config_toml: &ConfigToml,
        config: &Config,
        available_models: &[ModelPreset],
    ) -> ModelPreset {
        if let Some(preset) =
            self.official_openai_switcher_model(model, model_provider_id, available_models)
        {
            return preset;
        }

        self.configured_model_preset_from_config_toml(
            model,
            model_provider_id,
            config_toml,
            config,
            available_models,
        )
    }

    fn configured_model_entries(
        &self,
        config_toml: &ConfigToml,
        config: &Config,
    ) -> Vec<(String, Option<String>)> {
        let mut seen_models = HashSet::new();
        let mut configured_models = Vec::new();

        if let Some(model) = config
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
        {
            let provider_id = Some(config.model_provider_id.clone());
            let key = (model.to_string(), provider_id.clone());
            if seen_models.insert(key) {
                configured_models.push((model.to_string(), provider_id));
            }
        }

        let mut profile_models: Vec<(String, Option<String>)> = config_toml
            .profiles
            .values()
            .filter_map(|profile| {
                let profile_model = profile.model.as_deref()?.trim();
                if profile_model.is_empty() {
                    return None;
                }
                if let Ok(model_ref) = ModelRef::parse(profile_model) {
                    Some((
                        model_ref.model_id.clone(),
                        Some(model_provider_id_from_ref(&model_ref)),
                    ))
                } else {
                    Some((profile_model.to_string(), None))
                }
            })
            .collect();
        profile_models
            .sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));

        for (model, provider_id) in profile_models {
            let key = (model.clone(), provider_id.clone());
            if seen_models.insert(key) {
                configured_models.push((model, provider_id));
            }
        }

        if let Ok(models_json) = ModelsJson::load_from_adam_home(&config.adam_home) {
            for (model, provider_id) in models_json.model_entries() {
                let provider_id = Some(provider_id);
                let key = (model.clone(), provider_id.clone());
                if seen_models.insert(key) {
                    configured_models.push((model, provider_id));
                }
            }
        }

        configured_models
    }

    fn official_openai_switcher_model(
        &self,
        model: &str,
        model_provider_id: Option<&str>,
        available_models: &[ModelPreset],
    ) -> Option<ModelPreset> {
        if let Some(provider_id) = model_provider_id
            && provider_id != OPENAI_PROVIDER_ID
        {
            return None;
        }

        let mut preset = available_models
            .iter()
            .find(|candidate| {
                candidate.model == model
                    && candidate.model_provider_id.as_deref() == Some(OPENAI_PROVIDER_ID)
            })
            .cloned()?;
        preset.id = generated_provider_profile_name(OPENAI_PROVIDER_ID, model);
        preset.model_provider_id = Some(OPENAI_PROVIDER_ID.to_string());
        preset.description = OFFICIAL_OPENAI_PROVIDER_DESCRIPTION.to_string();
        preset.show_in_picker = true;
        preset.is_default = false;
        Some(preset)
    }

    fn apply_openai_official_switcher_metadata(&self, presets: &mut [ModelPreset]) {
        for preset in presets {
            if preset.model_provider_id.as_deref() == Some(OPENAI_PROVIDER_ID)
                && preset.id == preset.model
            {
                preset.description = OFFICIAL_OPENAI_PROVIDER_DESCRIPTION.to_string();
            }
        }
    }

    fn configured_picker_model(
        &self,
        config: &Config,
        picker_models: &[ModelPreset],
        available_models: &[ModelPreset],
    ) -> Option<ModelPreset> {
        let model = config.model.as_deref()?.trim();
        if model.is_empty() {
            return None;
        }

        let configured_model = if let Some(preset) = self.official_openai_switcher_model(
            model,
            Some(config.model_provider_id.as_str()),
            available_models,
        ) {
            preset
        } else if let Some(config_toml) = self.config_toml(config) {
            self.configured_model_preset_from_config_toml(
                model,
                Some(config.model_provider_id.as_str()),
                &config_toml,
                config,
                available_models,
            )
        } else {
            self.configured_model_from_config_toml(model, config, available_models)
        };

        if Self::picker_contains_model_identity(picker_models, &configured_model) {
            return None;
        }

        Some(configured_model)
    }

    fn is_current_provider_openai(&self) -> bool {
        self.model_provider_id
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_str()
            == OPENAI_PROVIDER_ID
    }

    fn configured_model_from_config_toml(
        &self,
        model: &str,
        config: &Config,
        available_models: &[ModelPreset],
    ) -> ModelPreset {
        if let Some(existing) = available_models.iter().find(|preset| preset.model == model) {
            let mut preset = existing.clone();
            preset.show_in_picker = true;
            preset.is_default = false;
            return preset;
        }

        let model_info =
            model_info::with_config_overrides(model_info::find_model_info_for_slug(model), config);
        let default_reasoning_effort =
            configured_model_default_reasoning_effort(config, &model_info);
        let supports_personality = model_info.supports_personality();

        ModelPreset {
            id: model.to_string(),
            model: model.to_string(),
            model_provider_id: None,
            display_name: model.to_string(),
            description: "Configured model from config.toml.".to_string(),
            default_reasoning_effort,
            supported_reasoning_efforts: model_info.supported_reasoning_levels,
            supports_personality,
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
        }
    }

    fn configured_model_preset_from_config_toml(
        &self,
        model: &str,
        model_provider_id: Option<&str>,
        config_toml: &ConfigToml,
        config: &Config,
        available_models: &[ModelPreset],
    ) -> ModelPreset {
        if let Some(provider_id) = model_provider_id {
            return self.configured_provider_model_from_config_toml(
                model,
                provider_id,
                config_toml,
                config,
                available_models,
            );
        }

        self.configured_model_from_config_toml(model, config, available_models)
    }

    fn configured_provider_model_from_config_toml(
        &self,
        model: &str,
        provider_id: &str,
        _config_toml: &ConfigToml,
        config: &Config,
        available_models: &[ModelPreset],
    ) -> ModelPreset {
        let id = generated_provider_profile_name(provider_id, model);
        let is_user_defined_provider = provider_id != OPENAI_PROVIDER_ID
            && ModelsJson::load_from_adam_home(&config.adam_home)
                .ok()
                .is_some_and(|models_json| {
                    models_json
                        .model_entries()
                        .into_iter()
                        .any(|(_, configured_provider_id)| configured_provider_id == provider_id)
                });
        let description = if is_user_defined_provider {
            format!(
                "User-defined model from {} provider.",
                display_model_provider_ref(provider_id)
            )
        } else {
            format!(
                "Configured model from {} provider.",
                display_model_provider_ref(provider_id)
            )
        };

        let mut preset = available_models
            .iter()
            .find(|candidate| {
                candidate.model == model
                    && candidate.model_provider_id.as_deref() == Some(provider_id)
            })
            .cloned()
            .or_else(|| {
                available_models
                    .iter()
                    .find(|candidate| candidate.model == model)
                    .cloned()
            })
            .unwrap_or_else(|| {
                let model_info = model_info::with_config_overrides(
                    model_info::find_model_info_for_slug(model),
                    config,
                );
                let default_reasoning_effort =
                    configured_model_default_reasoning_effort(config, &model_info);
                let supports_personality = model_info.supports_personality();

                ModelPreset {
                    id: model.to_string(),
                    model: model.to_string(),
                    model_provider_id: None,
                    display_name: model.to_string(),
                    description: String::new(),
                    default_reasoning_effort,
                    supported_reasoning_efforts: model_info.supported_reasoning_levels,
                    supports_personality,
                    is_default: false,
                    upgrade: None,
                    show_in_picker: true,
                    supported_in_api: true,
                }
            });
        preset.id = id;
        preset.model_provider_id = Some(provider_id.to_string());
        preset.description = description;
        preset.show_in_picker = true;
        preset.is_default = false;
        preset
    }

    fn same_model_identity(left: &ModelPreset, right: &ModelPreset) -> bool {
        left.model == right.model && left.model_provider_id == right.model_provider_id
    }

    fn assign_available_model_provider_identity(&self, mut preset: ModelPreset) -> ModelPreset {
        if preset.model_provider_id.is_some() {
            return preset;
        }

        preset.model_provider_id = Some(
            if self.is_builtin_model_slug(&preset.model)
                || self.is_current_provider_openai()
                || self.is_chatgpt_official_remote_model_slug(&preset.model)
            {
                OPENAI_PROVIDER_ID.to_string()
            } else {
                self.current_provider_id()
            },
        );
        preset
    }

    fn assign_builtin_model_provider_identity(&self, mut preset: ModelPreset) -> ModelPreset {
        if preset.model_provider_id.is_none() {
            preset.model_provider_id = Some(OPENAI_PROVIDER_ID.to_string());
        }
        preset
    }

    fn current_provider_id(&self) -> String {
        self.model_provider_id
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn is_builtin_model_slug(&self, model: &str) -> bool {
        self.local_models.iter().any(|preset| preset.model == model)
    }

    fn is_chatgpt_official_remote_model_slug(&self, model: &str) -> bool {
        matches!(
            self.auth_manager.get_internal_auth_mode(),
            Some(AuthMode::Chatgpt)
        ) && BUNDLED_REMOTE_MODEL_SLUGS.contains(model)
    }

    fn config_toml(&self, config: &Config) -> Option<ConfigToml> {
        config.config_layer_stack.effective_config().try_into().ok()
    }
}

fn configured_model_default_reasoning_effort(
    config: &Config,
    model_info: &ModelInfo,
) -> ReasoningEffort {
    config
        .model_reasoning_effort
        .or(model_info.default_reasoning_level)
        .unwrap_or({
            if model_info.supported_reasoning_levels.is_empty() {
                ReasoningEffort::None
            } else {
                ReasoningEffort::Medium
            }
        })
}

impl ModelsManager {
    pub fn is_configured_custom_model(
        model: &str,
        config: &Config,
        auth_mode: Option<AuthMode>,
    ) -> bool {
        let model = model.trim();
        let Some(config_model) = config
            .model
            .as_deref()
            .map(str::trim)
            .filter(|configured_model| !configured_model.is_empty())
        else {
            return false;
        };

        if config_model != model {
            return false;
        }

        let local_presets = builtin_model_presets(auth_mode);
        let remote_presets: Vec<ModelPreset> = Self::load_remote_models_from_file()
            .map(|response_models| response_models.into_iter().map(Into::into).collect())
            .unwrap_or_default();
        let picker_models = ModelPreset::filter_by_auth(
            ModelPreset::merge(remote_presets, local_presets),
            matches!(auth_mode, Some(AuthMode::Chatgpt)),
        )
        .into_iter()
        .filter(|preset| preset.show_in_picker)
        .collect::<Vec<_>>();

        !picker_models
            .iter()
            .any(|preset| preset.model == config_model)
    }

    async fn get_remote_models(&self, config: &Config) -> Vec<ModelInfo> {
        if config.features.enabled(Feature::RemoteModels)
            && Self::provider_uses_model_catalog(&self.provider_snapshot())
        {
            self.remote_models.read().await.clone()
        } else {
            Vec::new()
        }
    }

    fn try_get_remote_models(&self, config: &Config) -> Result<Vec<ModelInfo>, TryLockError> {
        if config.features.enabled(Feature::RemoteModels)
            && Self::provider_uses_model_catalog(&self.provider_snapshot())
        {
            Ok(self.remote_models.try_read()?.clone())
        } else {
            Ok(Vec::new())
        }
    }

    fn provider_snapshot(&self) -> RuntimeEndpoint {
        self.provider
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn provider_uses_model_catalog(provider: &RuntimeEndpoint) -> bool {
        provider.supports_model_catalog()
    }

    fn builtin_presets_for_provider(&self) -> Vec<ModelPreset> {
        if Self::provider_uses_model_catalog(&self.provider_snapshot()) {
            self.local_models.clone()
        } else {
            Vec::new()
        }
    }

    async fn clear_remote_model_state(&self) {
        *self.remote_models.write().await = Vec::new();
        *self.etag.write().await = None;
    }

    fn cache_manager_snapshot(&self) -> ModelsCacheManager {
        self.cache_manager
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[cfg(any(test, feature = "test-support"))]
    /// Construct a manager with a specific provider for testing.
    pub fn with_provider(
        adam_home: PathBuf,
        auth_manager: Arc<AuthManager>,
        model_provider_id: &str,
        provider: RuntimeEndpoint,
    ) -> Self {
        Self::new(adam_home, auth_manager, model_provider_id, provider)
    }

    #[cfg(any(test, feature = "test-support"))]
    /// Get model identifier without consulting remote state or cache.
    pub fn get_model_offline(model: Option<&str>) -> String {
        if let Some(model) = model {
            return model.to_string();
        }
        let presets = builtin_model_presets(None);
        presets
            .iter()
            .find(|preset| preset.show_in_picker)
            .or_else(|| presets.first())
            .map(|preset| preset.model.clone())
            .unwrap_or_default()
    }

    #[cfg(any(test, feature = "test-support"))]
    /// Build `ModelInfo` without consulting remote state or cache.
    pub fn construct_model_info_offline(model: &str, config: &Config) -> ModelInfo {
        model_info::with_config_overrides(model_info::find_model_info_for_slug(model), config)
    }
}

fn models_cache_path(adam_home: &std::path::Path, model_provider_id: &str) -> PathBuf {
    adam_home
        .join("remote_models")
        .join(model_provider_cache_key(model_provider_id))
        .join(MODEL_CACHE_FILE)
}

/// Convert a client version string to a whole version string (e.g. "1.2.3-alpha.4" -> "1.2.3")
fn format_client_version_to_whole() -> String {
    format!(
        "{}.{}.{}",
        env!("CARGO_PKG_VERSION_MAJOR"),
        env!("CARGO_PKG_VERSION_MINOR"),
        env!("CARGO_PKG_VERSION_PATCH")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CodexAuth;
    use crate::auth::AuthCredentialsStoreMode;
    use crate::config::ConfigBuilder;
    use crate::features::Feature;
    use adam_protocol::openai_models::ModelsResponse;
    use chrono::Utc;
    use core_test_support::responses::mount_models_once;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use tempfile::tempdir;
    use wiremock::MockServer;

    fn remote_model(slug: &str, display: &str, priority: i32) -> ModelInfo {
        remote_model_with_visibility(slug, display, priority, "list")
    }

    fn remote_model_with_visibility(
        slug: &str,
        display: &str,
        priority: i32,
        visibility: &str,
    ) -> ModelInfo {
        serde_json::from_value(json!({
            "slug": slug,
            "display_name": display,
            "description": format!("{display} desc"),
            "default_reasoning_level": "medium",
            "supported_reasoning_levels": [{"effort": "low", "description": "low"}, {"effort": "medium", "description": "medium"}],
            "shell_type": "shell_command",
            "visibility": visibility,
            "minimal_client_version": [0, 1, 0],
            "supported_in_api": true,
            "priority": priority,
            "upgrade": null,
            "base_instructions": "base instructions",
            "supports_reasoning_summaries": false,
            "support_verbosity": false,
            "default_verbosity": null,
            "apply_patch_tool_type": null,
            "truncation_policy": {"mode": "bytes", "limit": 10_000},
            "supports_parallel_tool_calls": false,
            "context_window": 272_000,
            "experimental_supported_tools": [],
        }))
        .expect("valid model")
    }

    fn assert_models_contain(actual: &[ModelInfo], expected: &[ModelInfo]) {
        for model in expected {
            assert!(
                actual.iter().any(|candidate| candidate.slug == model.slug),
                "expected model {} in cached list",
                model.slug
            );
        }
    }

    fn provider_for(base_url: String) -> RuntimeEndpoint {
        RuntimeEndpoint::openai_compatible_responses("mock", base_url)
            .with_request_max_retries(Some(0))
            .with_stream_max_retries(Some(0))
            .with_stream_idle_timeout_ms(Some(5_000))
    }

    fn messages_provider_for(base_url: String) -> RuntimeEndpoint {
        let mut provider = provider_for(base_url);
        provider.set_message_turns();
        provider.env_key = Some("ANTHROPIC_API_KEY".to_string());
        provider
    }

    async fn load_config_from_toml(adam_home: &tempfile::TempDir, config_toml: &str) -> Config {
        let config_toml = prepare_legacy_model_fixture(adam_home.path(), config_toml);
        tokio::fs::write(adam_home.path().join("config.toml"), config_toml)
            .await
            .expect("write config.toml");
        ConfigBuilder::default()
            .adam_home(adam_home.path().to_path_buf())
            .build()
            .await
            .expect("load test config")
    }

    fn prepare_legacy_model_fixture(adam_home: &std::path::Path, config_toml: &str) -> String {
        let mut doc = config_toml
            .parse::<toml_edit::DocumentMut>()
            .expect("test config should parse");
        let original = config_toml
            .parse::<toml::Table>()
            .expect("test config should parse as value");
        let mut models_json = json!({ "providers": {} });

        if let Some(model_providers) = original
            .get("model_providers")
            .and_then(toml::Value::as_table)
        {
            for (provider_id, provider) in model_providers {
                if let Some(variants) = provider.get("variants").and_then(toml::Value::as_table) {
                    for (endpoint_id, endpoint) in variants {
                        insert_models_endpoint(
                            &mut models_json,
                            provider_id,
                            endpoint_id,
                            endpoint,
                        );
                    }
                } else {
                    insert_models_endpoint(&mut models_json, provider_id, "main", provider);
                }
            }
        }

        let top_model = original
            .get("model")
            .and_then(toml::Value::as_str)
            .map(str::to_string);
        let top_provider = original
            .get("model_provider")
            .and_then(toml::Value::as_str)
            .unwrap_or(OPENAI_PROVIDER_ID)
            .to_string();
        if let Some(model) = top_model.as_deref() {
            insert_models_entry(&mut models_json, &top_provider, model, None);
            write_test_state_json(adam_home, &top_provider, model);
        }

        if let Some(profiles) = doc["profiles"].as_table_mut() {
            for (_, profile) in profiles.iter_mut() {
                let Some(profile_table) = profile.as_table_mut() else {
                    continue;
                };
                let model = profile_table
                    .get("model")
                    .and_then(toml_edit::Item::as_str)
                    .map(str::to_string);
                let provider = profile_table
                    .get("model_provider")
                    .and_then(toml_edit::Item::as_str)
                    .map(str::to_string);
                let context_window = profile_table
                    .get("model_context_window")
                    .and_then(toml_edit::Item::as_integer);
                if let Some(model) = model
                    && let Some(provider) = provider.as_deref()
                {
                    profile_table["model"] = toml_edit::value(model_ref_string(provider, &model));
                    insert_models_entry(&mut models_json, provider, &model, context_window);
                }
                profile_table.remove("model_provider");
                profile_table.remove("model_context_window");
            }
        }

        doc.as_table_mut().remove("model");
        doc.as_table_mut().remove("model_provider");
        doc.as_table_mut().remove("model_providers");

        if models_json["providers"]
            .as_object()
            .is_some_and(|providers| !providers.is_empty())
        {
            std::fs::write(
                adam_home.join("models.json"),
                serde_json::to_string_pretty(&models_json).expect("serialize models fixture"),
            )
            .expect("write models.json");
        }

        doc.to_string()
    }

    fn insert_models_endpoint(
        models_json: &mut serde_json::Value,
        provider_id: &str,
        endpoint_id: &str,
        endpoint: &toml::Value,
    ) {
        let provider = &mut models_json["providers"][provider_id];
        if provider.is_null() {
            *provider = json!({ "name": provider_id, "endpoints": {} });
        }
        let base_url = endpoint.get("base_url").and_then(toml::Value::as_str);
        let dialect = endpoint
            .get("dialect")
            .and_then(toml::Value::as_str)
            .unwrap_or("chat");
        let bearer = endpoint
            .get("experimental_bearer_token")
            .and_then(toml::Value::as_str);
        let env_key = endpoint.get("env_key").and_then(toml::Value::as_str);
        let endpoint_value = &mut provider["endpoints"][endpoint_id];
        if endpoint_value.is_null() {
            *endpoint_value = json!({ "name": provider_id, "dialect": dialect, "models": {} });
        }
        endpoint_value["dialect"] = json!(dialect);
        if let Some(base_url) = base_url {
            endpoint_value["base_url"] = json!(base_url);
        }
        if let Some(bearer) = bearer {
            endpoint_value["experimental_bearer_token"] = json!(bearer);
        }
        if let Some(env_key) = env_key {
            endpoint_value["env_key"] = json!(env_key);
        }
    }

    fn insert_models_entry(
        models_json: &mut serde_json::Value,
        provider_ref: &str,
        model: &str,
        context_window: Option<i64>,
    ) {
        let (provider_id, endpoint_id) = provider_ref
            .rsplit_once('.')
            .unwrap_or((provider_ref, "main"));
        let provider = &mut models_json["providers"][provider_id];
        if provider.is_null() {
            *provider = json!({ "name": provider_id, "endpoints": {} });
        }
        let endpoint = &mut provider["endpoints"][endpoint_id];
        if endpoint.is_null() {
            *endpoint = json!({ "name": provider_id, "dialect": "chat", "models": {} });
        }
        endpoint["models"][model] = if let Some(context_window) = context_window {
            json!({ "context_window": context_window })
        } else if endpoint["models"][model].is_null() {
            json!({})
        } else {
            endpoint["models"][model].clone()
        };
    }

    fn write_test_state_json(adam_home: &std::path::Path, provider_ref: &str, model: &str) {
        std::fs::write(
            adam_home.join("state.json"),
            serde_json::to_string_pretty(&json!({
                "last_selected_model": {
                    "model_ref": model_ref_string(provider_ref, model),
                    "selected_at": null,
                },
                "last_reasoning_effort": null,
                "last_model_verbosity": null,
            }))
            .expect("serialize state fixture"),
        )
        .expect("write state.json");
    }

    fn model_ref_string(provider_ref: &str, model: &str) -> String {
        if provider_ref.contains('.') {
            format!("{provider_ref}:{model}")
        } else {
            format!("{provider_ref}.main:{model}")
        }
    }

    #[tokio::test]
    async fn refresh_available_models_sorts_by_priority() {
        core_test_support::skip_if_sandbox!();

        let server = MockServer::start().await;
        let remote_models = vec![
            remote_model("priority-low", "Low", 1),
            remote_model("priority-high", "High", 0),
        ];
        let models_mock = mount_models_once(
            &server,
            ModelsResponse {
                models: remote_models.clone(),
            },
        )
        .await;

        let adam_home = tempdir().expect("temp dir");
        let mut config = ConfigBuilder::default()
            .adam_home(adam_home.path().to_path_buf())
            .build()
            .await
            .expect("load default test config");
        config.features.enable(Feature::RemoteModels);
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let provider = provider_for(server.uri());
        let manager = ModelsManager::with_provider(
            adam_home.path().to_path_buf(),
            auth_manager,
            "mock-provider",
            provider,
        );

        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("refresh succeeds");
        let cached_remote = manager.get_remote_models(&config).await;
        assert_models_contain(&cached_remote, &remote_models);

        let available = manager
            .list_models(&config, RefreshStrategy::OnlineIfUncached)
            .await;
        let high_idx = available
            .iter()
            .position(|model| model.model == "priority-high")
            .expect("priority-high should be listed");
        let low_idx = available
            .iter()
            .position(|model| model.model == "priority-low")
            .expect("priority-low should be listed");
        assert!(
            high_idx < low_idx,
            "higher priority should be listed before lower priority"
        );
        assert_eq!(
            models_mock.requests().len(),
            1,
            "expected a single /models request"
        );
    }

    #[tokio::test]
    async fn new_uses_supplied_provider_for_remote_model_refresh() {
        core_test_support::skip_if_sandbox!();

        let server = MockServer::start().await;
        let remote_models = vec![remote_model("custom-provider-model", "Custom Provider", 1)];
        let models_mock = mount_models_once(
            &server,
            ModelsResponse {
                models: remote_models.clone(),
            },
        )
        .await;

        let adam_home = tempdir().expect("temp dir");
        let mut config = ConfigBuilder::default()
            .adam_home(adam_home.path().to_path_buf())
            .build()
            .await
            .expect("load default test config");
        config.features.enable(Feature::RemoteModels);
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let provider = provider_for(server.uri());
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "custom-provider",
            provider,
        );

        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("refresh succeeds");
        assert_models_contain(&manager.get_remote_models(&config).await, &remote_models);
        assert_eq!(
            models_mock.requests().len(),
            1,
            "expected a single /models request against the supplied provider"
        );
    }

    #[tokio::test]
    async fn refresh_available_models_uses_cache_when_fresh() {
        core_test_support::skip_if_sandbox!();

        let server = MockServer::start().await;
        let remote_models = vec![remote_model("cached", "Cached", 5)];
        let models_mock = mount_models_once(
            &server,
            ModelsResponse {
                models: remote_models.clone(),
            },
        )
        .await;

        let adam_home = tempdir().expect("temp dir");
        let mut config = ConfigBuilder::default()
            .adam_home(adam_home.path().to_path_buf())
            .build()
            .await
            .expect("load default test config");
        config.features.enable(Feature::RemoteModels);
        let auth_manager = Arc::new(AuthManager::new(
            adam_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let provider = provider_for(server.uri());
        let manager = ModelsManager::with_provider(
            adam_home.path().to_path_buf(),
            auth_manager,
            "mock-provider",
            provider,
        );

        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("first refresh succeeds");
        assert_models_contain(&manager.get_remote_models(&config).await, &remote_models);

        // Second call should read from cache and avoid the network.
        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("cached refresh succeeds");
        assert_models_contain(&manager.get_remote_models(&config).await, &remote_models);
        assert_eq!(
            models_mock.requests().len(),
            1,
            "cache hit should avoid a second /models request"
        );
    }

    #[tokio::test]
    async fn refresh_available_models_scopes_cache_by_provider() {
        core_test_support::skip_if_sandbox!();

        let server_a = MockServer::start().await;
        let models_a = vec![remote_model("provider-a", "Provider A", 1)];
        let mock_a = mount_models_once(
            &server_a,
            ModelsResponse {
                models: models_a.clone(),
            },
        )
        .await;

        let server_b = MockServer::start().await;
        let models_b = vec![remote_model("provider-b", "Provider B", 1)];
        let mock_b = mount_models_once(
            &server_b,
            ModelsResponse {
                models: models_b.clone(),
            },
        )
        .await;

        let adam_home = tempdir().expect("temp dir");
        let mut config = ConfigBuilder::default()
            .adam_home(adam_home.path().to_path_buf())
            .build()
            .await
            .expect("load default test config");
        config.features.enable(Feature::RemoteModels);
        let auth_manager = Arc::new(AuthManager::new(
            adam_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));

        let manager_a = ModelsManager::with_provider(
            adam_home.path().to_path_buf(),
            Arc::clone(&auth_manager),
            "mock-provider-a",
            provider_for(server_a.uri()),
        );
        manager_a
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("provider A refresh succeeds");
        assert_models_contain(&manager_a.get_remote_models(&config).await, &models_a);

        let manager_b = ModelsManager::with_provider(
            adam_home.path().to_path_buf(),
            auth_manager,
            "mock-provider-b",
            provider_for(server_b.uri()),
        );
        manager_b
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("provider B refresh succeeds");

        let remote_models = manager_b.get_remote_models(&config).await;
        assert_models_contain(&remote_models, &models_b);
        assert!(
            !remote_models.iter().any(|model| model.slug == "provider-a"),
            "provider B should not reuse provider A cache"
        );
        assert_eq!(
            mock_a.requests().len(),
            1,
            "provider A should fetch /models once"
        );
        assert_eq!(
            mock_b.requests().len(),
            1,
            "provider B should fetch /models once instead of reusing provider A cache"
        );
    }

    #[tokio::test]
    async fn refresh_available_models_refetches_when_cache_stale() {
        core_test_support::skip_if_sandbox!();

        let server = MockServer::start().await;
        let initial_models = vec![remote_model("stale", "Stale", 1)];
        let initial_mock = mount_models_once(
            &server,
            ModelsResponse {
                models: initial_models.clone(),
            },
        )
        .await;

        let adam_home = tempdir().expect("temp dir");
        let mut config = ConfigBuilder::default()
            .adam_home(adam_home.path().to_path_buf())
            .build()
            .await
            .expect("load default test config");
        config.features.enable(Feature::RemoteModels);
        let auth_manager = Arc::new(AuthManager::new(
            adam_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let provider = provider_for(server.uri());
        let manager = ModelsManager::with_provider(
            adam_home.path().to_path_buf(),
            auth_manager,
            "mock-provider",
            provider,
        );

        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("initial refresh succeeds");

        // Rewrite cache with an old timestamp so it is treated as stale.
        let cache_manager = manager
            .cache_manager
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        cache_manager
            .manipulate_cache_for_test(|fetched_at| {
                *fetched_at = Utc::now() - chrono::Duration::hours(1);
            })
            .await
            .expect("cache manipulation succeeds");

        let updated_models = vec![remote_model("fresh", "Fresh", 9)];
        server.reset().await;
        let refreshed_mock = mount_models_once(
            &server,
            ModelsResponse {
                models: updated_models.clone(),
            },
        )
        .await;

        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("second refresh succeeds");
        assert_models_contain(&manager.get_remote_models(&config).await, &updated_models);
        assert_eq!(
            initial_mock.requests().len(),
            1,
            "initial refresh should only hit /models once"
        );
        assert_eq!(
            refreshed_mock.requests().len(),
            1,
            "stale cache refresh should fetch /models once"
        );
    }

    #[tokio::test]
    async fn refresh_available_models_drops_removed_remote_models() {
        core_test_support::skip_if_sandbox!();

        let server = MockServer::start().await;
        let initial_models = vec![remote_model("remote-old", "Remote Old", 1)];
        let initial_mock = mount_models_once(
            &server,
            ModelsResponse {
                models: initial_models,
            },
        )
        .await;

        let adam_home = tempdir().expect("temp dir");
        let mut config = ConfigBuilder::default()
            .adam_home(adam_home.path().to_path_buf())
            .build()
            .await
            .expect("load default test config");
        config.features.enable(Feature::RemoteModels);
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let provider = provider_for(server.uri());
        let manager = ModelsManager::with_provider(
            adam_home.path().to_path_buf(),
            auth_manager,
            "mock-provider",
            provider,
        );
        manager
            .cache_manager
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_ttl(Duration::ZERO);

        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("initial refresh succeeds");

        server.reset().await;
        let refreshed_models = vec![remote_model("remote-new", "Remote New", 1)];
        let refreshed_mock = mount_models_once(
            &server,
            ModelsResponse {
                models: refreshed_models,
            },
        )
        .await;

        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("second refresh succeeds");

        let available = manager
            .try_list_models(&config)
            .expect("models should be available");
        assert!(
            available.iter().any(|preset| preset.model == "remote-new"),
            "new remote model should be listed"
        );
        assert!(
            !available.iter().any(|preset| preset.model == "remote-old"),
            "removed remote model should not be listed"
        );
        assert_eq!(
            initial_mock.requests().len(),
            1,
            "initial refresh should only hit /models once"
        );
        assert_eq!(
            refreshed_mock.requests().len(),
            1,
            "second refresh should only hit /models once"
        );
    }

    #[test]
    fn build_available_models_picks_default_after_hiding_hidden_models() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::from_api_key("Test API Key"));
        let provider = provider_for("http://example.test".to_string());
        let mut manager = ModelsManager::with_provider(
            adam_home.path().to_path_buf(),
            auth_manager,
            "mock-provider",
            provider,
        );
        manager.local_models = Vec::new();

        let hidden_model = remote_model_with_visibility("hidden", "Hidden", 0, "hide");
        let visible_model = remote_model_with_visibility("visible", "Visible", 1, "list");

        let mut expected_hidden = ModelPreset::from(hidden_model.clone());
        expected_hidden.model_provider_id = Some("mock-provider".to_string());
        let mut expected_visible = ModelPreset::from(visible_model.clone());
        expected_visible.model_provider_id = Some("mock-provider".to_string());
        expected_visible.is_default = true;

        let available = manager.build_available_models(vec![hidden_model, visible_model]);

        assert_eq!(available, vec![expected_hidden, expected_visible]);
    }

    #[test]
    fn bundled_models_json_roundtrips() {
        let file_contents = include_str!("../../models.json");
        let response: ModelsResponse =
            serde_json::from_str(file_contents).expect("bundled models.json should deserialize");

        let serialized =
            serde_json::to_string(&response).expect("bundled models.json should serialize");
        let roundtripped: ModelsResponse =
            serde_json::from_str(&serialized).expect("serialized models.json should deserialize");

        assert_eq!(
            response, roundtripped,
            "bundled models.json should round trip through serde"
        );
        assert!(
            !response.models.is_empty(),
            "bundled models.json should contain at least one model"
        );
    }

    #[test]
    fn models_cache_path_uses_readable_prefix_and_hash() {
        let path = models_cache_path(std::path::Path::new("/tmp/adam"), "mock/provider:beta");
        assert_eq!(
            path,
            PathBuf::from(
                "/tmp/adam/remote_models/mock_provider_beta__70b0afe22d/models_cache.json"
            )
        );
    }

    #[test]
    fn models_cache_path_avoids_variant_collisions() {
        let base = std::path::Path::new("/tmp/adam");
        let plain = models_cache_path(base, "acme_chat");
        let variant = models_cache_path(base, "acme.chat");

        assert_ne!(plain, variant);
    }

    #[tokio::test]
    async fn list_model_switcher_models_without_auth_returns_only_configured_custom_model() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager = Arc::new(AuthManager::new(
            adam_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "mock-model"
"#,
        )
        .await;

        let picker_models = manager
            .list_model_switcher_models(&config, RefreshStrategy::Offline)
            .await;

        assert_eq!(picker_models.len(), 1);
        assert_eq!(picker_models[0].model, "mock-model");
        assert_eq!(picker_models[0].display_name, "mock-model");
        assert_eq!(
            picker_models[0].description,
            "Configured model from openai provider."
        );
        assert_eq!(
            picker_models[0].default_reasoning_effort,
            ReasoningEffort::None
        );
        assert!(picker_models[0].supported_reasoning_efforts.is_empty());
        assert!(picker_models[0].is_default);
        assert!(picker_models[0].show_in_picker);
    }

    #[tokio::test]
    async fn list_model_switcher_models_without_chatgpt_auth_returns_all_models_in_config_toml() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager = Arc::new(AuthManager::new(
            adam_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "mock-model"

[profiles.fast]
model = "deepseek-r1"

[profiles.other]
model_provider = "other-provider"
model = "claude-sonnet"

[profiles.duplicate]
model = "deepseek-r1"
"#,
        )
        .await;

        let picker_models = manager
            .list_model_switcher_models(&config, RefreshStrategy::Offline)
            .await;
        let models = picker_models
            .iter()
            .map(|preset| preset.model.as_str())
            .collect::<Vec<_>>();

        assert_eq!(models, vec!["mock-model", "claude-sonnet", "deepseek-r1"]);
        assert_eq!(
            picker_models
                .iter()
                .filter(|preset| preset.is_default)
                .count(),
            1
        );
        assert_eq!(picker_models[0].model, "mock-model");
        assert!(picker_models[0].is_default);
    }

    #[tokio::test]
    async fn list_model_switcher_models_without_chatgpt_auth_keeps_same_model_for_custom_providers()
    {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager = Arc::new(AuthManager::new(
            adam_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "gpt-5.2"
model_provider = "provider_a"

[model_providers.provider_a]
name = "provider_a"
base_url = "https://example.com/a"
dialect = "chat"
experimental_bearer_token = "sk-a"

[model_providers.provider_b]
name = "provider_b"
base_url = "https://example.com/b"
dialect = "chat"
experimental_bearer_token = "sk-b"

[profiles.second]
model = "gpt-5.2"
model_provider = "provider_b"

[profiles.duplicate]
model = "gpt-5.2"
model_provider = "provider_b"
"#,
        )
        .await;

        let picker_models = manager
            .list_model_switcher_models(&config, RefreshStrategy::Offline)
            .await;

        assert_eq!(picker_models.len(), 2);
        assert_eq!(
            picker_models
                .iter()
                .map(|preset| {
                    (
                        preset.model.as_str(),
                        preset.model_provider_id.as_deref(),
                        preset.description.as_str(),
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                (
                    "gpt-5.2",
                    Some("provider_a"),
                    "User-defined model from provider_a provider.",
                ),
                (
                    "gpt-5.2",
                    Some("provider_b"),
                    "User-defined model from provider_b provider.",
                ),
            ]
        );
        assert!(picker_models[0].is_default);
        assert!(!picker_models[1].is_default);
        assert!(picker_models.iter().all(|preset| preset.id != preset.model));
        assert!(
            picker_models
                .iter()
                .all(|preset| !preset.supported_reasoning_efforts.is_empty())
        );
    }

    #[tokio::test]
    async fn list_model_switcher_models_keeps_same_model_for_provider_variants() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager = Arc::new(AuthManager::new(
            adam_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "claude-sonnet-4-5"
model_provider = "anthropic.messages"

[model_providers.anthropic.variants.messages]
name = "anthropic"
base_url = "https://api.anthropic.com/v1"
dialect = "messages"
experimental_bearer_token = "sk-msg"

[model_providers.anthropic.variants.chat]
name = "anthropic"
base_url = "https://example.com/chat"
dialect = "chat"
experimental_bearer_token = "sk-chat"

[profiles.chat]
model = "claude-sonnet-4-5"
model_provider = "anthropic.chat"
"#,
        )
        .await;

        let picker_models = manager
            .list_model_switcher_models(&config, RefreshStrategy::Offline)
            .await;

        assert_eq!(
            picker_models
                .iter()
                .filter(|preset| preset.model == "claude-sonnet-4-5")
                .map(|preset| {
                    (
                        preset.model_provider_id.as_deref(),
                        preset.description.as_str(),
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                (
                    Some("anthropic.messages"),
                    "User-defined model from anthropic (messages) provider.",
                ),
                (
                    Some("anthropic.chat"),
                    "User-defined model from anthropic (chat) provider.",
                ),
            ]
        );
    }

    #[tokio::test]
    async fn list_model_switcher_models_without_chatgpt_auth_preserves_configured_provider_ids() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager = Arc::new(AuthManager::new(
            adam_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "gpt-5.2"
model_provider = "openai"

[model_providers.anthropic.variants.messages]
name = "anthropic"
base_url = "https://api.anthropic.com/v1"
dialect = "messages"
experimental_bearer_token = "sk-msg"

[model_providers.anthropic.variants.chat]
name = "anthropic"
base_url = "https://example.com/chat"
dialect = "chat"
experimental_bearer_token = "sk-chat"

[profiles.messages]
model = "gpt-5.2"
model_provider = "anthropic.messages"

[profiles.chat]
model = "gpt-5.2"
model_provider = "anthropic.chat"

[profiles.duplicate]
model = "gpt-5.2"
model_provider = "anthropic.messages"
"#,
        )
        .await;

        let picker_models = manager
            .list_model_switcher_models(&config, RefreshStrategy::Offline)
            .await;

        assert_eq!(picker_models.len(), 3);
        assert_eq!(
            picker_models
                .iter()
                .map(|preset| (
                    preset.id.clone(),
                    preset.model.clone(),
                    preset.model_provider_id.clone(),
                    preset.description.clone(),
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    generated_provider_profile_name("openai", "gpt-5.2"),
                    "gpt-5.2".to_string(),
                    Some("openai".to_string()),
                    "Configured model from openai provider.".to_string(),
                ),
                (
                    generated_provider_profile_name("anthropic.chat", "gpt-5.2"),
                    "gpt-5.2".to_string(),
                    Some("anthropic.chat".to_string()),
                    "User-defined model from anthropic (chat) provider.".to_string(),
                ),
                (
                    generated_provider_profile_name("anthropic.messages", "gpt-5.2"),
                    "gpt-5.2".to_string(),
                    Some("anthropic.messages".to_string()),
                    "User-defined model from anthropic (messages) provider.".to_string(),
                ),
            ]
        );
        assert!(picker_models[0].is_default);
        assert!(!picker_models[1].is_default);
        assert!(!picker_models[2].is_default);
        assert!(picker_models.iter().all(|preset| preset.id != preset.model));
    }

    #[tokio::test]
    async fn list_model_switcher_models_with_auth_appends_configured_models() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "mock-model"

[profiles.fast]
model = "deepseek-r1"
"#,
        )
        .await;

        let picker_models = manager
            .list_model_switcher_models(&config, RefreshStrategy::Offline)
            .await;

        assert_eq!(
            picker_models.first().map(|preset| preset.model.as_str()),
            Some("gpt-5.3-codex")
        );
        assert_eq!(
            picker_models.last().map(|preset| preset.model.as_str()),
            Some("deepseek-r1")
        );
        assert_eq!(
            picker_models
                .iter()
                .filter(|preset| preset.is_default)
                .count(),
            1
        );
        assert!(
            picker_models
                .iter()
                .any(|preset| preset.model == "gpt-5.3-codex" && preset.is_default)
        );
        assert!(
            picker_models
                .iter()
                .any(|preset| preset.model == "mock-model")
        );
        assert_eq!(
            picker_models
                .iter()
                .find(|preset| preset.model == "mock-model")
                .map(|preset| preset.description.as_str()),
            Some("Configured model from openai provider.")
        );
    }

    #[tokio::test]
    async fn list_model_switcher_models_with_chatgpt_auth_keeps_same_model_for_custom_provider() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "gpt-5.2"
model_provider = "provider_a"

[model_providers.provider_a]
name = "provider_a"
base_url = "https://example.com/a"
dialect = "chat"
experimental_bearer_token = "sk-a"
"#,
        )
        .await;

        let picker_models = manager
            .list_model_switcher_models(&config, RefreshStrategy::Offline)
            .await;

        let matching_models = picker_models
            .iter()
            .filter(|preset| preset.model == "gpt-5.2")
            .map(|preset| {
                (
                    preset.id.clone(),
                    preset.model_provider_id.clone(),
                    preset.description.clone(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(matching_models.len(), 2);
        assert_eq!(matching_models[0].0, "gpt-5.2".to_string());
        assert_eq!(matching_models[0].1, Some("openai".to_string()));
        assert_eq!(
            matching_models[0].2,
            OFFICIAL_OPENAI_PROVIDER_DESCRIPTION.to_string()
        );
        assert_eq!(
            matching_models[1],
            (
                generated_provider_profile_name("provider_a", "gpt-5.2"),
                Some("provider_a".to_string()),
                "User-defined model from provider_a provider.".to_string(),
            )
        );
    }

    #[tokio::test]
    async fn list_model_switcher_models_with_chatgpt_auth_keeps_custom_same_slug_when_active() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "gpt-5.2"
model_provider = "provider_a"

[model_providers.provider_a]
name = "provider_a"
base_url = "https://example.com/a"
dialect = "chat"
experimental_bearer_token = "sk-a"
"#,
        )
        .await;

        let provider = config
            .model_providers
            .get("provider_a")
            .cloned()
            .expect("provider_a should exist in config");
        manager.switch_provider("provider_a", provider).await;

        let picker_models = manager
            .list_model_switcher_models(&config, RefreshStrategy::Offline)
            .await;

        let matching_models = picker_models
            .iter()
            .filter(|preset| preset.model == "gpt-5.2")
            .map(|preset| {
                (
                    preset.id.clone(),
                    preset.model_provider_id.clone(),
                    preset.description.clone(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(matching_models.len(), 2);
        assert_eq!(matching_models[0].0, "gpt-5.2".to_string());
        assert_eq!(matching_models[0].1, Some("openai".to_string()));
        assert_eq!(
            matching_models[0].2,
            OFFICIAL_OPENAI_PROVIDER_DESCRIPTION.to_string()
        );
        assert_eq!(
            matching_models[1],
            (
                generated_provider_profile_name("provider_a", "gpt-5.2"),
                Some("provider_a".to_string()),
                "User-defined model from provider_a provider.".to_string(),
            )
        );
    }

    #[tokio::test]
    async fn list_model_switcher_models_with_chatgpt_auth_keeps_remote_official_and_custom_same_slug_when_active()
     {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "gpt-5.4"
model_provider = "provider_a"

[model_providers.provider_a]
name = "provider_a"
base_url = "https://example.com/a"
dialect = "chat"
experimental_bearer_token = "sk-a"
"#,
        )
        .await;

        let provider = config
            .model_providers
            .get("provider_a")
            .cloned()
            .expect("provider_a should exist in config");
        manager.switch_provider("provider_a", provider).await;

        let available_models =
            manager.build_available_models(vec![remote_model("gpt-5.4", "gpt-5.4", 1)]);
        let picker_models = manager.build_model_switcher_models(&config, available_models);

        let matching_models = picker_models
            .iter()
            .filter(|preset| preset.model == "gpt-5.4")
            .map(|preset| {
                (
                    preset.id.clone(),
                    preset.model_provider_id.clone(),
                    preset.description.clone(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            matching_models,
            vec![
                (
                    "gpt-5.4".to_string(),
                    Some("openai".to_string()),
                    OFFICIAL_OPENAI_PROVIDER_DESCRIPTION.to_string(),
                ),
                (
                    generated_provider_profile_name("provider_a", "gpt-5.4"),
                    Some("provider_a".to_string()),
                    "User-defined model from provider_a provider.".to_string(),
                ),
            ]
        );
    }

    #[tokio::test]
    async fn list_model_switcher_models_with_chatgpt_auth_maps_providerless_official_models_to_openai()
     {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "gpt-5.4"

[profiles.custom]
model = "gpt-5.4"
model_provider = "provider_a"

[model_providers.provider_a]
name = "provider_a"
base_url = "https://example.com/a"
dialect = "chat"
experimental_bearer_token = "sk-a"
"#,
        )
        .await;

        let available_models =
            manager.build_available_models(vec![remote_model("gpt-5.4", "gpt-5.4", 1)]);
        let picker_models = manager.build_model_switcher_models(&config, available_models);

        let matching_models = picker_models
            .iter()
            .filter(|preset| preset.model == "gpt-5.4")
            .map(|preset| {
                (
                    preset.id.clone(),
                    preset.model_provider_id.clone(),
                    preset.description.clone(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            matching_models,
            vec![
                (
                    "gpt-5.4".to_string(),
                    Some("openai".to_string()),
                    OFFICIAL_OPENAI_PROVIDER_DESCRIPTION.to_string(),
                ),
                (
                    generated_provider_profile_name("provider_a", "gpt-5.4"),
                    Some("provider_a".to_string()),
                    "User-defined model from provider_a provider.".to_string(),
                ),
            ]
        );
    }

    #[tokio::test]
    async fn list_model_switcher_models_with_chatgpt_auth_keeps_unknown_openai_models_configured() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "custom-openai-model"
model_provider = "openai"
"#,
        )
        .await;

        let available_models =
            manager.build_available_models(vec![remote_model("gpt-5.4", "gpt-5.4", 1)]);
        let picker_models = manager.build_model_switcher_models(&config, available_models);

        let custom_model = picker_models
            .iter()
            .find(|preset| preset.model == "custom-openai-model")
            .expect("custom OpenAI model should be present in switcher");

        assert_eq!(
            (
                custom_model.id.clone(),
                custom_model.model_provider_id.clone(),
                custom_model.description.clone(),
            ),
            (
                generated_provider_profile_name("openai", "custom-openai-model"),
                Some("openai".to_string()),
                "Configured model from openai provider.".to_string(),
            )
        );
    }

    #[tokio::test]
    async fn try_is_official_openai_model_returns_true_for_official_openai_model() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "gpt-5.2"
model_provider = "openai"
"#,
        )
        .await;

        let is_official = manager
            .try_is_official_openai_model(&config, "gpt-5.2", "openai")
            .expect("official model check");

        assert!(is_official);
    }

    #[tokio::test]
    async fn try_is_official_openai_model_returns_false_for_custom_provider_variant() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "gpt-5.2"
model_provider = "provider_a"

[model_providers.provider_a]
name = "provider_a"
base_url = "https://example.com/a"
dialect = "chat"
experimental_bearer_token = "sk-a"
"#,
        )
        .await;

        let is_official = manager
            .try_is_official_openai_model(&config, "gpt-5.2", "provider_a")
            .expect("official model check");

        assert!(!is_official);
    }

    #[tokio::test]
    async fn try_is_official_openai_model_returns_false_for_unknown_openai_model() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "custom-openai-model"
model_provider = "openai"
"#,
        )
        .await;

        let is_official = manager
            .try_is_official_openai_model(&config, "custom-openai-model", "openai")
            .expect("official model check");

        assert!(!is_official);
    }

    #[tokio::test]
    async fn list_picker_models_with_chatgpt_auth_normalizes_visible_official_openai_model() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "gpt-5.4"

[profiles.custom]
model = "gpt-5.4"
model_provider = "provider_a"

[model_providers.provider_a]
name = "provider_a"
base_url = "https://example.com/a"
dialect = "chat"
experimental_bearer_token = "sk-a"
"#,
        )
        .await;

        let available_models =
            manager.build_available_models(vec![remote_model("gpt-5.4", "gpt-5.4", 1)]);
        let picker_models = manager.build_picker_models(&config, available_models);

        let matching_models = picker_models
            .iter()
            .filter(|preset| preset.model == "gpt-5.4")
            .map(|preset| {
                (
                    preset.id.clone(),
                    preset.model_provider_id.clone(),
                    preset.description.clone(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            matching_models,
            vec![(
                "gpt-5.4".to_string(),
                Some("openai".to_string()),
                OFFICIAL_OPENAI_PROVIDER_DESCRIPTION.to_string(),
            )]
        );
    }

    #[tokio::test]
    async fn list_picker_models_with_chatgpt_auth_keeps_unknown_openai_models_configured() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "custom-openai-model"
model_provider = "openai"
"#,
        )
        .await;

        let available_models =
            manager.build_available_models(vec![remote_model("gpt-5.4", "gpt-5.4", 1)]);
        let picker_models = manager.build_picker_models(&config, available_models);

        let custom_model = picker_models
            .iter()
            .find(|preset| preset.model == "custom-openai-model")
            .expect("custom OpenAI model should be present in picker");

        assert_eq!(
            (
                custom_model.id.clone(),
                custom_model.model_provider_id.clone(),
                custom_model.description.clone(),
            ),
            (
                generated_provider_profile_name("openai", "custom-openai-model"),
                Some("openai".to_string()),
                "Configured model from openai provider.".to_string(),
            )
        );
    }

    #[tokio::test]
    async fn list_picker_models_with_chatgpt_auth_keeps_same_slug_custom_provider() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "gpt-5.2"
model_provider = "provider_a"

[model_providers.provider_a]
name = "provider_a"
base_url = "https://example.com/a"
dialect = "chat"
experimental_bearer_token = "sk-a"
"#,
        )
        .await;

        let picker_models = manager
            .list_picker_models(&config, RefreshStrategy::Offline)
            .await;

        let matching_models = picker_models
            .iter()
            .filter(|preset| preset.model == "gpt-5.2")
            .map(|preset| {
                (
                    preset.id.clone(),
                    preset.model_provider_id.clone(),
                    preset.description.clone(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            matching_models,
            vec![
                (
                    "gpt-5.2".to_string(),
                    Some("openai".to_string()),
                    OFFICIAL_OPENAI_PROVIDER_DESCRIPTION.to_string(),
                ),
                (
                    generated_provider_profile_name("provider_a", "gpt-5.2"),
                    Some("provider_a".to_string()),
                    "User-defined model from provider_a provider.".to_string(),
                ),
            ]
        );
    }

    #[tokio::test]
    async fn list_picker_models_without_remote_models_uses_builtin_gpt_5_3_codex_default() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
[features]
remote_models = false
"#,
        )
        .await;

        let picker_models = manager
            .list_picker_models(&config, RefreshStrategy::Offline)
            .await;

        assert_eq!(
            picker_models.first().map(|preset| preset.model.as_str()),
            Some("gpt-5.3-codex")
        );
        assert!(
            picker_models
                .iter()
                .any(|preset| preset.model == "gpt-5.3-codex" && preset.is_default)
        );
    }

    #[tokio::test]
    async fn get_default_model_without_remote_models_uses_builtin_gpt_5_3_codex() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
[features]
remote_models = false
"#,
        )
        .await;

        let model = manager
            .get_default_model(&None, &config, RefreshStrategy::Offline)
            .await
            .expect("offline default model should resolve");

        assert_eq!(model, "gpt-5.3-codex");
    }

    #[tokio::test]
    async fn list_model_switcher_models_with_chatgpt_auth_and_remote_models_disabled_includes_official_openai_models()
     {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "claude-sonnet-4-5"
model_provider = "provider_a"

[features]
remote_models = false

[model_providers.provider_a]
name = "provider_a"
base_url = "https://example.com/a"
dialect = "chat"
experimental_bearer_token = "sk-a"
"#,
        )
        .await;

        let switcher_models = manager
            .list_model_switcher_models(&config, RefreshStrategy::Offline)
            .await;

        assert!(switcher_models.iter().any(|preset| {
            preset.model == "gpt-5.4"
                && preset.model_provider_id.as_deref() == Some(OPENAI_PROVIDER_ID)
                && preset.description == OFFICIAL_OPENAI_PROVIDER_DESCRIPTION
        }));
        assert!(switcher_models.iter().any(|preset| {
            preset.model == "claude-sonnet-4-5"
                && preset.model_provider_id.as_deref() == Some("provider_a")
                && preset.description == "User-defined model from provider_a provider."
        }));
    }

    #[tokio::test]
    async fn list_model_switcher_models_with_chatgpt_auth_and_remote_models_disabled_keeps_custom_openai_model()
     {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "custom-openai-model"
model_provider = "openai"

[features]
remote_models = false
"#,
        )
        .await;

        let switcher_models = manager
            .list_model_switcher_models(&config, RefreshStrategy::Offline)
            .await;

        assert!(switcher_models.iter().any(|preset| {
            preset.model == "gpt-5.4"
                && preset.model_provider_id.as_deref() == Some(OPENAI_PROVIDER_ID)
                && preset.description == OFFICIAL_OPENAI_PROVIDER_DESCRIPTION
        }));
        assert!(switcher_models.iter().any(|preset| {
            preset.model == "custom-openai-model"
                && preset.model_provider_id.as_deref() == Some(OPENAI_PROVIDER_ID)
                && preset.description == "Configured model from openai provider."
        }));
    }

    #[tokio::test]
    async fn list_picker_models_with_chatgpt_auth_normalizes_hidden_official_openai_model() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "gpt-5.1"
"#,
        )
        .await;

        let picker_models = manager
            .list_picker_models(&config, RefreshStrategy::Offline)
            .await;

        let matching_models = picker_models
            .iter()
            .filter(|preset| preset.model == "gpt-5.1")
            .map(|preset| {
                (
                    preset.id.clone(),
                    preset.model_provider_id.clone(),
                    preset.description.clone(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            matching_models,
            vec![(
                generated_provider_profile_name("openai", "gpt-5.1"),
                Some("openai".to_string()),
                OFFICIAL_OPENAI_PROVIDER_DESCRIPTION.to_string(),
            )]
        );
    }

    #[tokio::test]
    async fn list_model_switcher_models_with_chatgpt_auth_dedupes_hidden_official_openai_model() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "gpt-5.1"
"#,
        )
        .await;

        let switcher_models = manager
            .list_model_switcher_models(&config, RefreshStrategy::Offline)
            .await;

        let matching_models = switcher_models
            .iter()
            .filter(|preset| preset.model == "gpt-5.1")
            .map(|preset| {
                (
                    preset.id.clone(),
                    preset.model_provider_id.clone(),
                    preset.description.clone(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            matching_models,
            vec![(
                generated_provider_profile_name("openai", "gpt-5.1"),
                Some("openai".to_string()),
                OFFICIAL_OPENAI_PROVIDER_DESCRIPTION.to_string(),
            )]
        );
    }

    #[tokio::test]
    async fn list_model_switcher_models_without_chatgpt_auth_preserves_providerless_top_level_entry()
     {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager = Arc::new(AuthManager::new(
            adam_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "shared-model"

[profiles.custom]
model = "shared-model"
model_provider = "provider_a"

[model_providers.provider_a]
name = "provider_a"
base_url = "https://example.com/a"
dialect = "chat"
experimental_bearer_token = "sk-a"
"#,
        )
        .await;

        let picker_models = manager
            .list_model_switcher_models(&config, RefreshStrategy::Offline)
            .await;

        let matching_models = picker_models
            .iter()
            .filter(|preset| preset.model == "shared-model")
            .map(|preset| {
                (
                    preset.id.clone(),
                    preset.model_provider_id.clone(),
                    preset.description.clone(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            matching_models,
            vec![
                (
                    generated_provider_profile_name("openai", "shared-model"),
                    Some("openai".to_string()),
                    "Configured model from openai provider.".to_string(),
                ),
                (
                    generated_provider_profile_name("provider_a", "shared-model"),
                    Some("provider_a".to_string()),
                    "User-defined model from provider_a provider.".to_string(),
                ),
            ]
        );
        assert!(picker_models[0].is_default);
    }

    #[tokio::test]
    async fn list_model_switcher_models_without_chatgpt_auth_keeps_ambiguous_providerless_entry() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager = Arc::new(AuthManager::new(
            adam_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "shared-model"

[profiles.first]
model = "shared-model"
model_provider = "provider_a"

[profiles.second]
model = "shared-model"
model_provider = "provider_b"

[model_providers.provider_a]
name = "provider_a"
base_url = "https://example.com/a"
dialect = "chat"
experimental_bearer_token = "sk-a"

[model_providers.provider_b]
name = "provider_b"
base_url = "https://example.com/b"
dialect = "chat"
experimental_bearer_token = "sk-b"
"#,
        )
        .await;

        let picker_models = manager
            .list_model_switcher_models(&config, RefreshStrategy::Offline)
            .await;

        let matching_models = picker_models
            .iter()
            .filter(|preset| preset.model == "shared-model")
            .map(|preset| {
                (
                    preset.id.clone(),
                    preset.model_provider_id.clone(),
                    preset.description.clone(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            matching_models,
            vec![
                (
                    generated_provider_profile_name("openai", "shared-model"),
                    Some("openai".to_string()),
                    "Configured model from openai provider.".to_string(),
                ),
                (
                    generated_provider_profile_name("provider_a", "shared-model"),
                    Some("provider_a".to_string()),
                    "User-defined model from provider_a provider.".to_string(),
                ),
                (
                    generated_provider_profile_name("provider_b", "shared-model"),
                    Some("provider_b".to_string()),
                    "User-defined model from provider_b provider.".to_string(),
                ),
            ]
        );
    }

    #[tokio::test]
    async fn list_model_switcher_models_with_chatgpt_auth_keeps_non_openai_catalog_descriptions() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let manager = ModelsManager::with_provider(
            adam_home.path().to_path_buf(),
            auth_manager,
            "mock-provider",
            provider_for("https://example.test".to_string()),
        );
        let config = load_config_from_toml(&adam_home, "").await;

        let available_models = manager.build_available_models(vec![remote_model(
            "custom-provider-model",
            "Custom Provider",
            1,
        )]);
        let picker_models = manager.build_model_switcher_models(&config, available_models);

        let custom_provider_model = picker_models
            .iter()
            .find(|preset| preset.model == "custom-provider-model")
            .expect("custom provider model should be present in switcher");

        assert_eq!(
            custom_provider_model.model_provider_id.as_deref(),
            Some("mock-provider")
        );
        assert_eq!(custom_provider_model.description, "Custom Provider desc");
    }

    #[tokio::test]
    async fn list_model_switcher_models_with_api_key_auth_returns_only_models_in_config_toml() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("sk-test"));
        let manager = ModelsManager::new(
            adam_home.path().to_path_buf(),
            auth_manager,
            "openai",
            RuntimeEndpoint::openai(),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "mock-model"
"#,
        )
        .await;

        let picker_models = manager
            .list_model_switcher_models(&config, RefreshStrategy::Offline)
            .await;

        assert_eq!(
            picker_models
                .iter()
                .map(|preset| preset.model.as_str())
                .collect::<Vec<_>>(),
            vec!["mock-model"]
        );
    }

    #[tokio::test]
    async fn messages_provider_starts_with_empty_remote_models() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager = Arc::new(AuthManager::new(
            adam_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let manager = ModelsManager::with_provider(
            adam_home.path().to_path_buf(),
            auth_manager,
            "mock-provider",
            messages_provider_for("http://example.test".to_string()),
        );
        let mut config = load_config_from_toml(&adam_home, "").await;
        config.features.enable(Feature::RemoteModels);

        assert!(manager.get_remote_models(&config).await.is_empty());
        assert!(
            manager
                .list_models(&config, RefreshStrategy::Offline)
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn switch_provider_to_messages_clears_remote_state() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager = Arc::new(AuthManager::new(
            adam_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let manager = ModelsManager::with_provider(
            adam_home.path().to_path_buf(),
            auth_manager,
            "mock-provider",
            provider_for("http://example.test".to_string()),
        );
        manager
            .apply_remote_models(vec![remote_model(
                "custom-provider-model",
                "Custom Provider",
                1,
            )])
            .await;
        *manager.etag.write().await = Some("etag-1".to_string());

        manager
            .switch_provider(
                "anthropic",
                messages_provider_for("https://api.anthropic.com/v1".to_string()),
            )
            .await;

        let mut config = load_config_from_toml(&adam_home, "").await;
        config.features.enable(Feature::RemoteModels);
        assert!(manager.get_remote_models(&config).await.is_empty());
        assert_eq!(manager.get_etag().await, None);
    }

    #[tokio::test]
    async fn list_picker_models_with_messages_provider_only_shows_configured_model() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager = Arc::new(AuthManager::new(
            adam_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let manager = ModelsManager::with_provider(
            adam_home.path().to_path_buf(),
            auth_manager,
            "anthropic",
            messages_provider_for("https://api.anthropic.com/v1".to_string()),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "claude-sonnet-4-5"
"#,
        )
        .await;

        let picker_models = manager
            .list_picker_models(&config, RefreshStrategy::Offline)
            .await;

        assert_eq!(picker_models.len(), 1);
        assert_eq!(picker_models[0].model, "claude-sonnet-4-5");
        assert!(picker_models[0].is_default);
    }

    #[tokio::test]
    async fn list_model_switcher_models_with_chatgpt_auth_and_messages_provider_includes_official_openai_models()
     {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let manager = ModelsManager::with_provider(
            adam_home.path().to_path_buf(),
            auth_manager,
            "anthropic.messages",
            messages_provider_for("https://api.anthropic.com/v1".to_string()),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "claude-sonnet-4-5"
model_provider = "anthropic.messages"

[model_providers.anthropic.variants.messages]
name = "anthropic"
base_url = "https://api.anthropic.com/v1"
dialect = "messages"
experimental_bearer_token = "sk-msg"
"#,
        )
        .await;

        let picker_models = manager
            .list_model_switcher_models(&config, RefreshStrategy::Offline)
            .await;

        assert!(picker_models.iter().any(|preset| {
            preset.model == "gpt-5.2-codex"
                && preset.model_provider_id.as_deref() == Some(OPENAI_PROVIDER_ID)
                && preset.description == OFFICIAL_OPENAI_PROVIDER_DESCRIPTION
        }));
        assert!(picker_models.iter().any(|preset| {
            preset.model == "claude-sonnet-4-5"
                && preset.model_provider_id.as_deref() == Some("anthropic.messages")
                && preset.description == "User-defined model from anthropic (messages) provider."
        }));
    }

    #[tokio::test]
    async fn list_model_switcher_models_with_chatgpt_auth_and_messages_provider_keeps_custom_same_slug()
     {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let manager = ModelsManager::with_provider(
            adam_home.path().to_path_buf(),
            auth_manager,
            "anthropic.messages",
            messages_provider_for("https://api.anthropic.com/v1".to_string()),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "gpt-5.2"
model_provider = "anthropic.messages"

[model_providers.anthropic.variants.messages]
name = "anthropic"
base_url = "https://api.anthropic.com/v1"
dialect = "messages"
experimental_bearer_token = "sk-msg"
"#,
        )
        .await;

        let picker_models = manager
            .list_model_switcher_models(&config, RefreshStrategy::Offline)
            .await;

        let matching_models = picker_models
            .iter()
            .filter(|preset| preset.model == "gpt-5.2")
            .map(|preset| {
                (
                    preset.id.clone(),
                    preset.model_provider_id.clone(),
                    preset.description.clone(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            matching_models,
            vec![
                (
                    "gpt-5.2".to_string(),
                    Some(OPENAI_PROVIDER_ID.to_string()),
                    OFFICIAL_OPENAI_PROVIDER_DESCRIPTION.to_string(),
                ),
                (
                    generated_provider_profile_name("anthropic.messages", "gpt-5.2"),
                    Some("anthropic.messages".to_string()),
                    "User-defined model from anthropic (messages) provider.".to_string(),
                ),
            ]
        );
    }

    #[tokio::test]
    async fn get_default_model_with_messages_provider_requires_explicit_model() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager = Arc::new(AuthManager::new(
            adam_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let manager = ModelsManager::with_provider(
            adam_home.path().to_path_buf(),
            auth_manager,
            "anthropic",
            messages_provider_for("https://api.anthropic.com/v1".to_string()),
        );
        let config = load_config_from_toml(&adam_home, "").await;

        let err = manager
            .get_default_model(&None, &config, RefreshStrategy::Offline)
            .await
            .expect_err("messages providers should require an explicit model");

        assert_eq!(
            err.to_string(),
            "Fatal error: dialect = \"messages\" requires an explicit model"
        );
    }

    #[tokio::test]
    async fn get_default_model_with_messages_provider_uses_configured_model() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager = Arc::new(AuthManager::new(
            adam_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let manager = ModelsManager::with_provider(
            adam_home.path().to_path_buf(),
            auth_manager,
            "anthropic",
            messages_provider_for("https://api.anthropic.com/v1".to_string()),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "claude-sonnet-4-5"
"#,
        )
        .await;

        let model = manager
            .get_default_model(&config.model, &config, RefreshStrategy::Offline)
            .await
            .expect("configured messages model should be accepted");

        assert_eq!(model, "claude-sonnet-4-5");
    }

    #[tokio::test]
    async fn list_model_switcher_models_with_provider_bearer_token_returns_only_models_in_config_toml()
     {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager = Arc::new(AuthManager::new(
            adam_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let mut provider = provider_for("http://example.test".to_string());
        provider.experimental_bearer_token = Some("sk-test".to_string());
        let manager = ModelsManager::with_provider(
            adam_home.path().to_path_buf(),
            auth_manager,
            "mock-provider",
            provider,
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "mock-model"
"#,
        )
        .await;

        let picker_models = manager
            .list_model_switcher_models(&config, RefreshStrategy::Offline)
            .await;

        assert_eq!(picker_models.len(), 1);
        assert_eq!(picker_models[0].model, "mock-model");
    }

    #[tokio::test]
    async fn list_model_switcher_models_with_provider_env_key_returns_only_models_in_config_toml() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager = Arc::new(AuthManager::new(
            adam_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let mut provider = provider_for("http://example.test".to_string());
        provider.env_key = Some("PATH".to_string());
        let manager = ModelsManager::with_provider(
            adam_home.path().to_path_buf(),
            auth_manager,
            "mock-provider",
            provider,
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "mock-model"
"#,
        )
        .await;

        let picker_models = manager
            .list_model_switcher_models(&config, RefreshStrategy::Offline)
            .await;

        assert_eq!(picker_models.len(), 1);
        assert_eq!(picker_models[0].model, "mock-model");
    }

    #[tokio::test]
    async fn set_provider_does_not_expand_model_switcher_without_chatgpt_auth() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager = Arc::new(AuthManager::new(
            adam_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let manager = ModelsManager::with_provider(
            adam_home.path().to_path_buf(),
            auth_manager,
            "mock-provider",
            provider_for("http://example.test".to_string()),
        );
        let config = load_config_from_toml(
            &adam_home,
            r#"
model = "mock-model"
"#,
        )
        .await;

        let picker_models = manager
            .list_model_switcher_models(&config, RefreshStrategy::Offline)
            .await;
        assert_eq!(picker_models.len(), 1);
        assert_eq!(picker_models[0].model, "mock-model");

        let mut updated_provider = provider_for("http://example.test/v2".to_string());
        updated_provider.experimental_bearer_token = Some("sk-test".to_string());
        manager.set_provider(updated_provider);

        let picker_models = manager
            .list_model_switcher_models(&config, RefreshStrategy::Offline)
            .await;
        assert_eq!(picker_models.len(), 1);
        assert_eq!(picker_models[0].model, "mock-model");
    }

    #[tokio::test]
    async fn configured_custom_model_detection_matches_picker_behavior() {
        let adam_home = tempdir().expect("temp dir");
        let auth_manager = Arc::new(AuthManager::new(
            adam_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let mut config = ConfigBuilder::default()
            .adam_home(adam_home.path().to_path_buf())
            .build()
            .await
            .expect("load default test config");

        config.model = Some("mock-model".to_string());
        assert!(ModelsManager::is_configured_custom_model(
            "mock-model",
            &config,
            auth_manager.get_internal_auth_mode(),
        ));

        config.model = Some("gpt-5.2-codex".to_string());
        assert!(!ModelsManager::is_configured_custom_model(
            "gpt-5.2-codex",
            &config,
            auth_manager.get_internal_auth_mode(),
        ));
    }
}
