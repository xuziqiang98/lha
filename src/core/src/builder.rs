use crate::manager::AgentManager;
use crate::skills::SkillProvider;
use crate::tools::ToolHandler;
use crate::tools::ToolRegistry;
use crate::tools::ToolRegistryBuilder;
use lha_llm::BaseInstructions;
use lha_llm::Personality;
use lha_llm::RuntimeMetadata;
use lha_llm::SemanticRuntime;
use serde_json::Value;
use std::sync::Arc;

pub struct AgentDefinition {
    pub(crate) runtime: Arc<dyn SemanticRuntime>,
    pub(crate) base_instructions: BaseInstructions,
    pub(crate) personality: Option<Personality>,
    pub(crate) output_schema: Option<Value>,
    pub(crate) tools: Arc<ToolRegistry>,
    pub(crate) runtime_metadata: RuntimeMetadata,
    pub(crate) skill_providers: Vec<Arc<dyn SkillProvider>>,
}

pub struct AgentBuilder {
    runtime: Arc<dyn SemanticRuntime>,
    base_instructions: BaseInstructions,
    personality: Option<Personality>,
    output_schema: Option<Value>,
    tools: ToolRegistryBuilder,
    skill_providers: Vec<Arc<dyn SkillProvider>>,
}

impl AgentBuilder {
    pub fn new(runtime: Arc<dyn SemanticRuntime>) -> Self {
        Self {
            base_instructions: BaseInstructions::default(),
            personality: None,
            output_schema: None,
            tools: ToolRegistryBuilder::new(),
            skill_providers: Vec::new(),
            runtime,
        }
    }

    pub fn with_base_instructions(mut self, text: impl Into<String>) -> Self {
        self.base_instructions = BaseInstructions { text: text.into() };
        self
    }

    pub fn with_personality(mut self, personality: Personality) -> Self {
        self.personality = Some(personality);
        self
    }

    pub fn with_output_schema(mut self, schema: Value) -> Self {
        self.output_schema = Some(schema);
        self
    }

    pub fn register_tool(mut self, handler: Arc<dyn ToolHandler>) -> Self {
        self.tools.register_handler(handler);
        self
    }

    pub fn register_skill_provider(mut self, provider: Arc<dyn SkillProvider>) -> Self {
        self.skill_providers.push(provider);
        self
    }

    pub fn build(self) -> AgentManager {
        let runtime_metadata = self.runtime.metadata();
        let definition = AgentDefinition {
            runtime: self.runtime,
            base_instructions: self.base_instructions,
            personality: self.personality,
            output_schema: self.output_schema,
            tools: Arc::new(self.tools.build()),
            runtime_metadata,
            skill_providers: self.skill_providers,
        };
        AgentManager::new(definition)
    }
}
