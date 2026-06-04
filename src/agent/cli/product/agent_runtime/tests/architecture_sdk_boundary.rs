use std::path::Path;

use crate::test_support::cargo_bin::find_resource;
use walkdir::WalkDir;

#[test]
fn agent_production_code_uses_llm_sdk_entrypoints() {
    let agent_src = find_resource!("product/agent_runtime/src")
        .expect("agent source directory should be available for architecture checks");
    let forbidden_patterns = [
        "crate::product::agent::model_provider_info",
        "lha_llm::ConversationDialect",
        "lha_llm::StreamingPreference",
        "lha_llm::provider::",
        "lha_llm::client::",
        "lha_llm::compatibility::",
        "lha_llm::prompt::",
        "lha_llm::transport::",
        "try_switch_fallback_transport",
        "RuntimeCapabilities::from_endpoint_and_model",
        "is_azure_responses_endpoint",
        "set_chat_completions_api",
        "set_responses_api",
        "set_messages_api",
        "with_chat_completions_api",
        "with_responses_api",
        "with_messages_api",
        "lha_llm::create_tools_json_for_",
    ];

    let offenders = WalkDir::new(&agent_src)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "rs"))
        .filter_map(|entry| find_forbidden_imports(&agent_src, entry.path(), &forbidden_patterns))
        .collect::<Vec<_>>();

    assert!(
        offenders.is_empty(),
        "agent code should depend on lha_llm semantic APIs only:\n{}",
        offenders.join("\n")
    );
}

fn find_forbidden_imports(
    agent_src: &Path,
    path: &Path,
    forbidden_patterns: &[&str],
) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    let matches = forbidden_patterns
        .iter()
        .filter(|pattern| !is_allowed_boundary_import(agent_src, path, pattern))
        .filter(|pattern| contents.contains(**pattern))
        .copied()
        .collect::<Vec<_>>();

    if matches.is_empty() {
        None
    } else {
        Some(format!("{} -> {}", path.display(), matches.join(", ")))
    }
}

fn is_allowed_boundary_import(agent_src: &Path, path: &Path, pattern: &str) -> bool {
    let Ok(rel_path) = path.strip_prefix(agent_src) else {
        return false;
    };

    rel_path == Path::new("default_client.rs") && pattern == "lha_llm::client::"
}
