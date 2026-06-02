use lha_agent::config::Config;
use lha_agent::features::Feature;
use ratatui::style::Stylize;
use ratatui::text::Line;

use crate::app_event::AppEvent;

use super::SelectionItem;
use super::SelectionViewParams;
use super::popup_consts::standard_popup_hint_line;

pub(crate) fn memories_settings_params(config: &Config) -> SelectionViewParams {
    let feature_enabled = config.features.enabled(Feature::MemoryTool);
    let use_memories = config.memories.use_memories;
    let generate_memories = config.memories.generate_memories;
    let dedicated_tools = config.memories.dedicated_tools;

    SelectionViewParams {
        title: Some("Memories".to_string()),
        subtitle: Some("Local file-backed memories are experimental.".to_string()),
        footer_note: Some(Line::from("Select a row to toggle it.".dim())),
        footer_hint: Some(standard_popup_hint_line()),
        items: vec![
            toggle_item(
                "Memory feature",
                feature_enabled,
                "Enable the memories feature flag.",
                (
                    !feature_enabled,
                    use_memories,
                    generate_memories,
                    dedicated_tools,
                ),
            ),
            toggle_item(
                "Use memories",
                use_memories,
                "Inject memory read instructions into new threads.",
                (
                    feature_enabled,
                    !use_memories,
                    generate_memories,
                    dedicated_tools,
                ),
            ),
            toggle_item(
                "Generate memories",
                generate_memories,
                "Allow new threads to be used by the write pipeline.",
                (
                    feature_enabled,
                    use_memories,
                    !generate_memories,
                    dedicated_tools,
                ),
            ),
            toggle_item(
                "Dedicated tools",
                dedicated_tools,
                "Expose memories__list/read/search/add_ad_hoc_note.",
                (
                    feature_enabled,
                    use_memories,
                    generate_memories,
                    !dedicated_tools,
                ),
            ),
        ],
        allow_background_transcript_interaction: true,
        ..Default::default()
    }
}

fn toggle_item(
    label: &str,
    enabled: bool,
    description: &str,
    next: (bool, bool, bool, bool),
) -> SelectionItem {
    let status = if enabled { "On" } else { "Off" };
    let (feature_enabled, use_memories, generate_memories, dedicated_tools) = next;
    SelectionItem {
        name: format!("{label}: {status}"),
        description: Some(description.to_string()),
        actions: vec![Box::new(move |tx| {
            tx.send(AppEvent::UpdateMemorySettings {
                feature_enabled,
                use_memories,
                generate_memories,
                dedicated_tools,
            });
        })],
        dismiss_on_select: true,
        ..Default::default()
    }
}
