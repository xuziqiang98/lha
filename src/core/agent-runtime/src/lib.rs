pub use lha_core::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reexports_runtime_surface() {
        assert!(matches!(SessionStatus::Idle, lha_core::SessionStatus::Idle));
        let _agent_builder_type = std::any::type_name::<AgentBuilder>();
    }
}
