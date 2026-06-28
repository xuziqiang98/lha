use crate::RunCollectTextError;
use crate::builder::AgentDefinition;
use crate::input::SessionInput;
use crate::session::AgentSession;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

pub struct AgentManager {
    definition: Arc<AgentDefinition>,
    next_session_id: AtomicU64,
}

impl AgentManager {
    pub(crate) fn new(definition: AgentDefinition) -> Self {
        Self {
            definition: Arc::new(definition),
            next_session_id: AtomicU64::new(1),
        }
    }

    pub fn create_session(&self) -> AgentSession {
        let session_id = self.next_session_id.fetch_add(1, Ordering::SeqCst);
        AgentSession::new(session_id, Arc::clone(&self.definition), Vec::new())
    }

    pub async fn ask_once(&self, text: impl Into<String>) -> Result<String, RunCollectTextError> {
        let session = self.create_session();
        session
            .run_collect_text(SessionInput::from_user_text(text))
            .await
    }
}
