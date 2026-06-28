//! Minimal agent-loop SDK primitives shared by higher-level agent products.

mod builder;
mod cancel;
mod error;
mod events;
mod input;
pub mod kernel;
mod manager;
#[cfg(feature = "mcp")]
pub mod mcp;
mod processor;
mod session;
pub mod skills;
mod snapshot;
mod status;
pub mod tools;

pub use builder::AgentBuilder;
pub use builder::AgentDefinition;
pub use error::Error;
pub use error::Result;
pub use error::RunCollectTextError;
pub use events::AgentEvent;
pub use events::TurnItemDelta;
pub use events::TurnSummary;
pub use input::InputQueue;
pub use input::SessionInput;
pub use manager::AgentManager;
pub use session::AgentSession;
pub use session::SessionId;
pub use session::SubmissionId;
pub use snapshot::ActiveTurnSnapshot;
pub use snapshot::SessionSnapshot;
pub use status::SessionStatus;
