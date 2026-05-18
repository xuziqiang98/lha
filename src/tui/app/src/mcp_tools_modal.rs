use std::collections::HashMap;

use adam_agent::config::Config;
use adam_agent::config::types::McpServerConfig;
use adam_agent::config::types::McpServerTransportConfig;
use adam_agent::protocol::McpAuthStatus;
use adam_agent::protocol::McpListToolsResponseEvent;
use adam_common::format_env_display::format_env_display;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use mcp_types::Resource;
use mcp_types::ResourceTemplate;
use mcp_types::Tool;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::BorderType;
use ratatui::widgets::Borders;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use textwrap::Options;
use textwrap::wrap;

use crate::render::Insets;
use crate::render::RectExt as _;

const MCP_DOCS_URL: &str = "https://developers.openai.com/codex/mcp";

pub(crate) struct McpToolsModal {
    servers: Vec<(String, McpServerConfig)>,
    state: McpToolsModalState,
    scroll_top: usize,
}

enum McpToolsModalState {
    Empty,
    Loading,
    Ready(McpToolsModalSnapshot),
}

struct McpToolsModalSnapshot {
    tools: HashMap<String, Tool>,
    resources: HashMap<String, Vec<Resource>>,
    resource_templates: HashMap<String, Vec<ResourceTemplate>>,
    auth_statuses: HashMap<String, McpAuthStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpToolsModalAction {
    None,
    Exit,
}

struct ModalRenderLines {
    header: Vec<Line<'static>>,
    body: Vec<Line<'static>>,
    footer: Vec<Line<'static>>,
}

impl McpToolsModal {
    pub(crate) fn new_empty(config: &Config) -> Self {
        Self {
            servers: sorted_servers(config),
            state: McpToolsModalState::Empty,
            scroll_top: 0,
        }
    }

    pub(crate) fn new_loading(config: &Config) -> Self {
        Self {
            servers: sorted_servers(config),
            state: McpToolsModalState::Loading,
            scroll_top: 0,
        }
    }

    pub(crate) fn set_snapshot(&mut self, config: &Config, response: McpListToolsResponseEvent) {
        self.servers = sorted_servers(config);
        self.state = McpToolsModalState::Ready(McpToolsModalSnapshot {
            tools: response.tools,
            resources: response.resources,
            resource_templates: response.resource_templates,
            auth_statuses: response.auth_statuses,
        });
        self.scroll_top = 0;
    }

    pub(crate) fn handle_key_event(&mut self, key_event: KeyEvent) -> McpToolsModalAction {
        if !matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return McpToolsModalAction::None;
        }

        match key_event {
            KeyEvent {
                code: KeyCode::Esc, ..
            }
            | KeyEvent {
                code: KeyCode::Char('c') | KeyCode::Char('d'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => McpToolsModalAction::Exit,
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('\u{0010}'),
                modifiers: KeyModifiers::NONE,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('k'),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.scroll_top = self.scroll_top.saturating_sub(1);
                McpToolsModalAction::None
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('\u{000e}'),
                modifiers: KeyModifiers::NONE,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('j'),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.scroll_top = self.scroll_top.saturating_add(1);
                McpToolsModalAction::None
            }
            KeyEvent {
                code: KeyCode::PageUp,
                ..
            } => {
                self.scroll_top = self.scroll_top.saturating_sub(8);
                McpToolsModalAction::None
            }
            KeyEvent {
                code: KeyCode::PageDown,
                ..
            } => {
                self.scroll_top = self.scroll_top.saturating_add(8);
                McpToolsModalAction::None
            }
            KeyEvent {
                code: KeyCode::Home,
                ..
            } => {
                self.scroll_top = 0;
                McpToolsModalAction::None
            }
            KeyEvent {
                code: KeyCode::End, ..
            } => {
                self.scroll_top = usize::MAX;
                McpToolsModalAction::None
            }
            _ => McpToolsModalAction::None,
        }
    }

    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        let modal_area = self.modal_area(area);
        Clear.render(modal_area, buf);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().add_modifier(Modifier::DIM));
        let inner_area = block.inner(modal_area);
        block.render(modal_area, buf);

        let content_area = inner_area.inset(Insets::vh(1, 2));
        if content_area.is_empty() {
            return;
        }

        let lines = self.render_lines(content_area.width.max(1) as usize);
        let footer_height = (lines.footer.len() as u16).min(content_area.height);
        let header_height =
            (lines.header.len() as u16).min(content_area.height.saturating_sub(footer_height));
        let body_height = content_area
            .height
            .saturating_sub(header_height)
            .saturating_sub(footer_height);

        if header_height > 0 {
            Paragraph::new(
                lines
                    .header
                    .iter()
                    .take(header_height as usize)
                    .cloned()
                    .collect::<Vec<_>>(),
            )
            .render(
                Rect {
                    height: header_height,
                    ..content_area
                },
                buf,
            );
        }

        if body_height > 0 {
            let max_scroll = lines.body.len().saturating_sub(body_height as usize);
            let scroll_top = self.scroll_top.min(max_scroll);
            Paragraph::new(
                lines
                    .body
                    .iter()
                    .skip(scroll_top)
                    .take(body_height as usize)
                    .cloned()
                    .collect::<Vec<_>>(),
            )
            .render(
                Rect {
                    y: content_area.y.saturating_add(header_height),
                    height: body_height,
                    ..content_area
                },
                buf,
            );
        }

        if footer_height > 0 {
            let footer_start = lines.footer.len().saturating_sub(footer_height as usize);
            Paragraph::new(
                lines
                    .footer
                    .iter()
                    .skip(footer_start)
                    .cloned()
                    .collect::<Vec<_>>(),
            )
            .render(
                Rect {
                    y: content_area
                        .y
                        .saturating_add(content_area.height.saturating_sub(footer_height)),
                    height: footer_height,
                    ..content_area
                },
                buf,
            );
        }
    }

    fn render_lines(&self, width: usize) -> ModalRenderLines {
        ModalRenderLines {
            header: vec![
                "MCP Tools".bold().into(),
                "Configured MCP servers, tools, resources, and authentication status."
                    .dim()
                    .into(),
                "".into(),
            ],
            body: self.body_lines(width),
            footer: vec![
                "".into(),
                vec![
                    "Up/Down/jk".cyan(),
                    " scroll   ".dim(),
                    "PgUp/PgDn".cyan(),
                    " page   ".dim(),
                    "Esc".cyan(),
                    " close".dim(),
                ]
                .into(),
            ],
        }
    }

    fn body_lines(&self, width: usize) -> Vec<Line<'static>> {
        match &self.state {
            McpToolsModalState::Empty => empty_lines(width),
            McpToolsModalState::Loading => loading_lines(&self.servers, width),
            McpToolsModalState::Ready(snapshot) => ready_lines(&self.servers, snapshot, width),
        }
    }

    fn content_height(&self, width: usize) -> usize {
        let lines = self.render_lines(width);
        lines
            .header
            .len()
            .saturating_add(lines.body.len())
            .saturating_add(lines.footer.len())
    }

    fn modal_area(&self, area: Rect) -> Rect {
        let width = area.width.saturating_sub(4).min(92).max(area.width.min(48));
        let content_width = width.saturating_sub(6).max(1) as usize;
        let desired_content_height = self.content_height(content_width);
        let desired_height = desired_content_height
            .saturating_add(4)
            .try_into()
            .unwrap_or(u16::MAX);
        let height = area
            .height
            .saturating_sub(2)
            .min(desired_height)
            .max(area.height.min(10));
        Rect {
            x: area.x + area.width.saturating_sub(width) / 2,
            y: area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        }
    }
}

fn sorted_servers(config: &Config) -> Vec<(String, McpServerConfig)> {
    let mut servers = config
        .mcp_servers
        .iter()
        .map(|(name, cfg)| (name.clone(), cfg.clone()))
        .collect::<Vec<_>>();
    servers.sort_by(|(a, _), (b, _)| a.cmp(b));
    servers
}

fn empty_lines(width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![];
    push_wrapped(
        &mut lines,
        "No MCP servers configured.",
        width,
        "  ",
        "  ",
        Some(Style::default().add_modifier(Modifier::ITALIC)),
    );
    lines.push(Line::from(""));
    push_wrapped(
        &mut lines,
        &format!("See the MCP docs to configure them: {MCP_DOCS_URL}"),
        width,
        "  ",
        "  ",
        Some(Style::default().add_modifier(Modifier::DIM)),
    );
    lines
}

fn loading_lines(servers: &[(String, McpServerConfig)], width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![];
    push_wrapped(&mut lines, "Loading MCP tools...", width, "  ", "  ", None);
    if servers.is_empty() {
        return lines;
    }

    lines.push(Line::from(""));
    lines.push("Configured servers".bold().into());
    for (server, cfg) in servers {
        let suffix = if cfg.enabled { "" } else { " (disabled)" };
        push_wrapped(
            &mut lines,
            &format!("- {server}{suffix}"),
            width,
            "  ",
            "    ",
            None,
        );
    }
    lines
}

fn ready_lines(
    servers: &[(String, McpServerConfig)],
    snapshot: &McpToolsModalSnapshot,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![];

    if snapshot.tools.is_empty() {
        push_wrapped(
            &mut lines,
            "No MCP tools available.",
            width,
            "  ",
            "  ",
            Some(Style::default().add_modifier(Modifier::ITALIC)),
        );
        lines.push(Line::from(""));
    }

    for (server, cfg) in servers {
        push_server_lines(&mut lines, server, cfg, snapshot, width);
        lines.push(Line::from(""));
    }

    if lines.is_empty() {
        empty_lines(width)
    } else {
        lines
    }
}

fn push_server_lines(
    lines: &mut Vec<Line<'static>>,
    server: &str,
    cfg: &McpServerConfig,
    snapshot: &McpToolsModalSnapshot,
    width: usize,
) {
    let mut header: Vec<Span<'static>> = vec!["- ".into(), server.to_string().bold()];
    if !cfg.enabled {
        header.push(" ".into());
        header.push("(disabled)".red());
    }
    lines.push(header.into());

    if !cfg.enabled {
        if let Some(reason) = cfg.disabled_reason.as_ref().map(ToString::to_string) {
            push_wrapped(
                lines,
                &format!("Reason: {reason}"),
                width,
                "    - ",
                "      ",
                None,
            );
        }
        return;
    }

    push_wrapped(lines, "Status: enabled", width, "    - ", "      ", None);
    let auth_status = snapshot
        .auth_statuses
        .get(server)
        .copied()
        .unwrap_or(McpAuthStatus::Unsupported);
    push_wrapped(
        lines,
        &format!("Auth: {auth_status}"),
        width,
        "    - ",
        "      ",
        None,
    );
    push_transport_lines(lines, &cfg.transport, width);
    push_tool_lines(lines, server, &snapshot.tools, width);
    push_resource_lines(lines, server, &snapshot.resources, width);
    push_resource_template_lines(lines, server, &snapshot.resource_templates, width);
}

fn push_transport_lines(
    lines: &mut Vec<Line<'static>>,
    transport: &McpServerTransportConfig,
    width: usize,
) {
    match transport {
        McpServerTransportConfig::Stdio {
            command,
            args,
            env,
            env_vars,
            cwd,
        } => {
            let args_suffix = if args.is_empty() {
                String::new()
            } else {
                format!(" {}", args.join(" "))
            };
            push_wrapped(
                lines,
                &format!("Command: {command}{args_suffix}"),
                width,
                "    - ",
                "      ",
                None,
            );

            if let Some(cwd) = cwd.as_ref() {
                push_wrapped(
                    lines,
                    &format!("Cwd: {}", cwd.display()),
                    width,
                    "    - ",
                    "      ",
                    None,
                );
            }

            let env_display = format_env_display(env.as_ref(), env_vars);
            if env_display != "-" {
                push_wrapped(
                    lines,
                    &format!("Env: {env_display}"),
                    width,
                    "    - ",
                    "      ",
                    None,
                );
            }
        }
        McpServerTransportConfig::StreamableHttp {
            url,
            bearer_token_env_var,
            http_headers,
            env_http_headers,
        } => {
            push_wrapped(
                lines,
                &format!("URL: {url}"),
                width,
                "    - ",
                "      ",
                None,
            );
            if let Some(var) = bearer_token_env_var.as_ref() {
                push_wrapped(
                    lines,
                    &format!("Bearer token env: {var}"),
                    width,
                    "    - ",
                    "      ",
                    None,
                );
            }
            if let Some(headers) = http_headers.as_ref()
                && !headers.is_empty()
            {
                let mut pairs = headers.iter().collect::<Vec<_>>();
                pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
                let display = pairs
                    .into_iter()
                    .map(|(name, _)| format!("{name}=*****"))
                    .collect::<Vec<_>>()
                    .join(", ");
                push_wrapped(
                    lines,
                    &format!("HTTP headers: {display}"),
                    width,
                    "    - ",
                    "      ",
                    None,
                );
            }
            if let Some(headers) = env_http_headers.as_ref()
                && !headers.is_empty()
            {
                let mut pairs = headers.iter().collect::<Vec<_>>();
                pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
                let display = pairs
                    .into_iter()
                    .map(|(name, var)| format!("{name}={var}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                push_wrapped(
                    lines,
                    &format!("Env HTTP headers: {display}"),
                    width,
                    "    - ",
                    "      ",
                    None,
                );
            }
        }
    }
}

fn push_tool_lines(
    lines: &mut Vec<Line<'static>>,
    server: &str,
    tools: &HashMap<String, Tool>,
    width: usize,
) {
    let prefix = format!("mcp__{server}__");
    let mut names = tools
        .keys()
        .filter(|name| name.starts_with(&prefix))
        .map(|name| name[prefix.len()..].to_string())
        .collect::<Vec<_>>();
    names.sort();

    let display = if names.is_empty() {
        "(none)".to_string()
    } else {
        names.join(", ")
    };
    push_wrapped(
        lines,
        &format!("Tools: {display}"),
        width,
        "    - ",
        "      ",
        None,
    );
}

fn push_resource_lines(
    lines: &mut Vec<Line<'static>>,
    server: &str,
    resources: &HashMap<String, Vec<Resource>>,
    width: usize,
) {
    let display = resources
        .get(server)
        .map(|resources| {
            resources
                .iter()
                .map(|resource| {
                    let label = resource.title.as_ref().unwrap_or(&resource.name);
                    format!("{label} ({})", resource.uri)
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|display| !display.is_empty())
        .unwrap_or_else(|| "(none)".to_string());
    push_wrapped(
        lines,
        &format!("Resources: {display}"),
        width,
        "    - ",
        "      ",
        None,
    );
}

fn push_resource_template_lines(
    lines: &mut Vec<Line<'static>>,
    server: &str,
    resource_templates: &HashMap<String, Vec<ResourceTemplate>>,
    width: usize,
) {
    let display = resource_templates
        .get(server)
        .map(|templates| {
            templates
                .iter()
                .map(|template| {
                    let label = template.title.as_ref().unwrap_or(&template.name);
                    format!("{label} ({})", template.uri_template)
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|display| !display.is_empty())
        .unwrap_or_else(|| "(none)".to_string());
    push_wrapped(
        lines,
        &format!("Resource templates: {display}"),
        width,
        "    - ",
        "      ",
        None,
    );
}

fn push_wrapped(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    width: usize,
    initial_indent: &str,
    subsequent_indent: &str,
    style: Option<Style>,
) {
    let wrap_width = width.max(1);
    let options = Options::new(wrap_width)
        .initial_indent(initial_indent)
        .subsequent_indent(subsequent_indent);
    for line in wrap(text, options) {
        let line = Line::from(line.into_owned());
        lines.push(match style {
            Some(style) => line.style(style),
            None => line,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use adam_agent::config::ConfigBuilder;
    use adam_agent::config::types::McpServerConfig;
    use mcp_types::Resource;
    use mcp_types::ResourceTemplate;
    use mcp_types::ToolInputSchema;
    use pretty_assertions::assert_eq;
    use ratatui::Terminal;

    use crate::test_backend::VT100Backend;

    async fn test_config() -> Config {
        ConfigBuilder::default()
            .adam_home(std::env::temp_dir())
            .build()
            .await
            .expect("config")
    }

    fn stdio_server(env: Option<HashMap<String, String>>) -> McpServerConfig {
        McpServerConfig {
            transport: McpServerTransportConfig::Stdio {
                command: "docs-server".to_string(),
                args: vec!["--port".to_string(), "1234".to_string()],
                env,
                env_vars: vec!["APP_TOKEN".to_string()],
                cwd: None,
            },
            enabled: true,
            disabled_reason: None,
            startup_timeout_sec: None,
            tool_timeout_sec: None,
            enabled_tools: None,
            disabled_tools: None,
            scopes: None,
        }
    }

    fn http_server() -> McpServerConfig {
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer secret".to_string());
        let mut env_headers = HashMap::new();
        env_headers.insert("X-API-Key".to_string(), "API_KEY_ENV".to_string());

        McpServerConfig {
            transport: McpServerTransportConfig::StreamableHttp {
                url: "https://example.com/mcp".to_string(),
                bearer_token_env_var: Some("MCP_TOKEN".to_string()),
                http_headers: Some(headers),
                env_http_headers: Some(env_headers),
            },
            enabled: true,
            disabled_reason: None,
            startup_timeout_sec: None,
            tool_timeout_sec: None,
            enabled_tools: None,
            disabled_tools: None,
            scopes: None,
        }
    }

    fn tool(name: &str) -> Tool {
        Tool {
            annotations: None,
            description: None,
            input_schema: ToolInputSchema {
                properties: None,
                required: None,
                r#type: "object".to_string(),
            },
            name: name.to_string(),
            output_schema: None,
            title: None,
        }
    }

    fn render_modal(modal: &McpToolsModal) -> String {
        let mut terminal = Terminal::new(VT100Backend::new(100, 32)).expect("terminal");
        terminal
            .draw(|frame| modal.render(frame.area(), frame.buffer_mut()))
            .expect("draw");
        terminal.backend().to_string()
    }

    #[tokio::test]
    async fn renders_loading_state() {
        let mut config = test_config().await;
        let mut servers = config.mcp_servers.get().clone();
        servers.insert("docs".to_string(), stdio_server(None));
        config.mcp_servers.set(servers).expect("mcp servers");

        let rendered = render_modal(&McpToolsModal::new_loading(&config));

        assert!(rendered.contains("MCP Tools"));
        assert!(rendered.contains("Loading MCP tools"));
        assert!(rendered.contains("docs"));
        assert!(rendered.contains("Esc"));
    }

    #[tokio::test]
    async fn renders_empty_state() {
        let config = test_config().await;
        let rendered = render_modal(&McpToolsModal::new_empty(&config));

        assert!(rendered.contains("No MCP servers configured"));
        assert!(rendered.contains("MCP docs"));
    }

    #[tokio::test]
    async fn renders_ready_state_and_masks_sensitive_values() {
        let mut config = test_config().await;
        let mut env = HashMap::new();
        env.insert("TOKEN".to_string(), "secret".to_string());
        let mut servers = config.mcp_servers.get().clone();
        servers.insert("docs".to_string(), stdio_server(Some(env)));
        servers.insert("http".to_string(), http_server());
        config.mcp_servers.set(servers).expect("mcp servers");

        let mut tools = HashMap::new();
        tools.insert("mcp__docs__list".to_string(), tool("list"));
        tools.insert("mcp__docs__search".to_string(), tool("search"));
        tools.insert("mcp__http__ping".to_string(), tool("ping"));

        let mut resources = HashMap::new();
        resources.insert(
            "docs".to_string(),
            vec![Resource {
                annotations: None,
                description: None,
                mime_type: None,
                name: "Guide".to_string(),
                size: None,
                title: None,
                uri: "docs://guide".to_string(),
            }],
        );
        let mut resource_templates = HashMap::new();
        resource_templates.insert(
            "docs".to_string(),
            vec![ResourceTemplate {
                annotations: None,
                description: None,
                mime_type: None,
                name: "Page".to_string(),
                title: None,
                uri_template: "docs://{page}".to_string(),
            }],
        );
        let mut auth_statuses = HashMap::new();
        auth_statuses.insert("docs".to_string(), McpAuthStatus::Unsupported);

        let mut modal = McpToolsModal::new_loading(&config);
        modal.set_snapshot(
            &config,
            McpListToolsResponseEvent {
                tools,
                resources,
                resource_templates,
                auth_statuses,
            },
        );
        let rendered = render_modal(&modal);

        assert!(rendered.contains("docs"));
        assert!(rendered.contains("http"));
        assert!(rendered.contains("docs-server --port 1234"));
        assert!(rendered.contains("TOKEN=*****"));
        assert!(!rendered.contains("secret"));
        assert!(rendered.contains("Authorization=*****"));
        assert!(rendered.contains("X-API-Key=API_KEY_ENV"));
        assert!(rendered.contains("list, search"));
        assert!(rendered.contains("Guide"));
        assert!(rendered.contains("Page"));
    }

    #[tokio::test]
    async fn escape_and_control_shortcuts_exit() {
        let config = test_config().await;
        let mut modal = McpToolsModal::new_empty(&config);

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            McpToolsModalAction::Exit
        );
        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            McpToolsModalAction::Exit
        );
        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            McpToolsModalAction::Exit
        );
    }
}
