use crate::SessionId;
use async_trait::async_trait;
use lha_llm::RuntimeMetadata;
use lha_llm::ToolDescriptor;
use lha_llm::TranscriptItem;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub instructions: String,
    pub required_tools: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SkillContext {
    pub session_id: SessionId,
    pub conversation: Vec<TranscriptItem>,
    pub runtime: RuntimeMetadata,
    pub tools: Vec<ToolDescriptor>,
}

#[derive(Debug, Error)]
pub enum SkillError {
    #[error("{0}")]
    Fatal(String),
}

#[async_trait]
pub trait SkillProvider: Send + Sync {
    async fn skills_for_turn(
        &self,
        context: &SkillContext,
    ) -> std::result::Result<Vec<Skill>, SkillError>;
}
