use crate::config::model_ref::ModelRef;
use crate::path_utils::write_atomically;
use lha_protocol::config_types::IdentityKind;
use lha_protocol::config_types::Verbosity;
use lha_protocol::openai_models::ReasoningEffort;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

pub const STATE_JSON_FILE: &str = "state.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct LHAStateJson {
    pub last_selected_model: Option<LastSelectedModel>,
    pub last_reasoning_effort: Option<ReasoningEffort>,
    pub last_model_verbosity: Option<Verbosity>,
    pub last_selected_identity: Option<IdentityKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct LastSelectedModel {
    pub model_ref: String,
    pub selected_at: Option<String>,
}

pub struct LHAStateStore {
    path: PathBuf,
}

impl LHAStateStore {
    pub fn new(lha_home: &Path) -> Self {
        Self {
            path: lha_home.join(STATE_JSON_FILE),
        }
    }

    pub fn load(&self) -> io::Result<LHAStateJson> {
        match std::fs::read_to_string(&self.path) {
            Ok(contents) => serde_json::from_str(&contents).map_err(|err| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("failed to parse {}: {err}", self.path.display()),
                )
            }),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(LHAStateJson::default()),
            Err(err) => Err(err),
        }
    }

    pub fn set_last_selected_model(
        &self,
        model_ref: &ModelRef,
        effort: Option<ReasoningEffort>,
        verbosity: Option<Verbosity>,
    ) -> io::Result<()> {
        let mut state = self.load()?;
        state.last_selected_model = Some(LastSelectedModel {
            model_ref: model_ref.to_string(),
            selected_at: OffsetDateTime::now_utc().format(&Rfc3339).ok(),
        });
        state.last_reasoning_effort = effort;
        if let Some(verbosity) = verbosity {
            state.last_model_verbosity = Some(verbosity);
        }
        let contents = serde_json::to_string_pretty(&state)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        write_atomically(&self.path, &format!("{contents}\n"))
    }

    pub fn set_last_selected_identity(&self, identity: IdentityKind) -> io::Result<()> {
        let mut state = self.load()?;
        state.last_selected_identity = Some(identity);
        let contents = serde_json::to_string_pretty(&state)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        write_atomically(&self.path, &format!("{contents}\n"))
    }
}

pub fn load_state(lha_home: &Path) -> io::Result<LHAStateJson> {
    LHAStateStore::new(lha_home).load()
}

#[cfg(test)]
mod tests {
    use super::LHAStateJson;
    use super::LHAStateStore;
    use super::LastSelectedModel;
    use super::STATE_JSON_FILE;
    use crate::config::model_ref::ModelRef;
    use lha_protocol::config_types::IdentityKind;
    use lha_protocol::openai_models::ReasoningEffort;
    use pretty_assertions::assert_eq;
    use std::io::ErrorKind;
    use tempfile::TempDir;

    #[test]
    fn missing_state_loads_empty() {
        let temp = TempDir::new().unwrap();
        let state = LHAStateStore::new(temp.path()).load().unwrap();
        assert_eq!(state, LHAStateJson::default());
    }

    #[test]
    fn writes_last_selected_model() {
        let temp = TempDir::new().unwrap();
        let store = LHAStateStore::new(temp.path());
        let model_ref = ModelRef::parse("openrouter.main:anthropic/claude-sonnet-4").unwrap();
        store
            .set_last_selected_model(&model_ref, None, None)
            .unwrap();
        let state = store.load().unwrap();
        assert_eq!(
            state.last_selected_model.unwrap().model_ref,
            model_ref.to_string()
        );
    }

    #[test]
    fn writes_last_selected_identity() {
        let temp = TempDir::new().unwrap();
        let store = LHAStateStore::new(temp.path());

        store
            .set_last_selected_identity(IdentityKind::Planner)
            .unwrap();

        let state = store.load().unwrap();
        assert_eq!(
            state,
            LHAStateJson {
                last_selected_identity: Some(IdentityKind::Planner),
                ..Default::default()
            }
        );
    }

    #[test]
    fn identity_write_preserves_model_selection() {
        let temp = TempDir::new().unwrap();
        let store = LHAStateStore::new(temp.path());
        let model_ref = ModelRef::parse("openrouter.main:anthropic/claude-sonnet-4").unwrap();
        store
            .set_last_selected_model(&model_ref, Some(ReasoningEffort::High), None)
            .unwrap();

        store
            .set_last_selected_identity(IdentityKind::Programmer)
            .unwrap();

        let state = store.load().unwrap();
        assert_eq!(
            state,
            LHAStateJson {
                last_selected_model: Some(LastSelectedModel {
                    model_ref: model_ref.to_string(),
                    selected_at: state
                        .last_selected_model
                        .as_ref()
                        .and_then(|selection| selection.selected_at.clone()),
                }),
                last_reasoning_effort: Some(ReasoningEffort::High),
                last_model_verbosity: None,
                last_selected_identity: Some(IdentityKind::Programmer),
            }
        );
    }

    #[test]
    fn model_write_preserves_identity() {
        let temp = TempDir::new().unwrap();
        let store = LHAStateStore::new(temp.path());
        let model_ref = ModelRef::parse("openrouter.main:anthropic/claude-sonnet-4").unwrap();
        store
            .set_last_selected_identity(IdentityKind::Planner)
            .unwrap();

        store
            .set_last_selected_model(&model_ref, None, None)
            .unwrap();

        let state = store.load().unwrap();
        assert_eq!(state.last_selected_identity, Some(IdentityKind::Planner));
        assert_eq!(
            state
                .last_selected_model
                .as_ref()
                .map(|selection| selection.model_ref.as_str()),
            Some(model_ref.to_string().as_str())
        );
    }

    #[test]
    fn parse_error_does_not_overwrite_state() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join(STATE_JSON_FILE);
        std::fs::write(&path, "{").unwrap();
        let store = LHAStateStore::new(temp.path());
        let model_ref = ModelRef::parse("openrouter.main:anthropic/claude-sonnet-4").unwrap();

        let err = store
            .set_last_selected_model(&model_ref, None, None)
            .unwrap_err();

        assert_eq!(err.kind(), ErrorKind::InvalidData);
        assert_eq!(std::fs::read_to_string(path).unwrap(), "{");
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join(STATE_JSON_FILE), r#"{"unknown":true}"#).unwrap();
        let err = LHAStateStore::new(temp.path()).load().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }
}
