//! Frame rendering: 3-pane layout (header, chat, input) plus overlays.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Clear, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Wrap,
};
use shell_key_manager::normalize_provider;
use tui_textarea::TextArea;

use crate::state::{
    ColorWheelState, InputMode, JsonImportState, SETUP_PROVIDERS, SetupStage, ThemePickerItem,
    ThemePickerMode, ThemesPicker, TuiState, command_description,
};
use crate::theme::Theme;

/// Rows reserved for the command palette (border + up to 5 entries).
const PALETTE_ROWS: u16 = 7;

/// Render the full app into `frame`.
///
/// Updates `state.scroll_offset` to follow the bottom of the chat unless the
/// user scrolled away, and styles `textarea` for the active theme/mode.
pub fn render_app(frame: &mut Frame, state: &mut TuiState, textarea: &mut TextArea<'static>) {
    let styles = state.theme.styles();
    let area = frame.area();
    frame.render_widget(Block::default().style(styles.base()), area);

    let palette_visible = state.command_palette && !state.matching_commands().is_empty();
    let palette_height = if palette_visible {
        PALETTE_ROWS.min(area.height.saturating_sub(8)).max(3)
    } else {
        0
    };

    let [header_area, chat_area, palette_area, input_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(palette_height),
        Constraint::Length(3),
    ])
    .margin(1)
    .areas(area);

    render_header(frame, state, header_area, &styles);
    render_chat(frame, state, chat_area);
    if palette_visible {
        render_palette(frame, state, palette_area);
    }
    render_input(frame, state, textarea, input_area);
    if state.show_history {
        render_history_overlay(frame, state, area);
    }
    if state.key_setup.is_some() {
        render_key_setup(frame, state, area);
    }
    if state.plan_review.is_some() {
        render_plan_review(state, frame, area);
    }
    if state.models_picker.is_some() {
        render_models_picker(frame, state, area);
    }
    if state.themes_picker.is_some() {
        render_themes_picker(frame, state, area);
    }
    if state.skills_picker.is_some() {
        render_skills_picker(frame, state, area);
    }
}

fn render_header(
    frame: &mut Frame,
    state: &TuiState,
    area: Rect,
    styles: &crate::theme::ThemeStyles,
) {
    let session = state
        .session_id
        .as_deref()
        .map(|id| id.chars().take(8).collect::<String>())
        .unwrap_or_else(|| "new session".to_string());
    let mut badges = Vec::new();
    if state.plan_pending {
        badges.push("[PLAN]");
    }
    if state.shell_mode {
        badges.push("[SHELL]");
    }
    let badge_text = if badges.is_empty() {
        String::new()
    } else {
        format!("  ·  {}", badges.join(" / "))
    };
    let working_status = state.working_status();
    let status = match (state.busy, working_status) {
        (true, Some(message)) => format!("  {} {message}", state.working_spinner()),
        (true, None) => format!("  {} Working…", state.working_spinner()),
        (false, _) => "  ·  Ready".to_string(),
    };
    let header = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("  ◇ S.I.C.K  ", styles.header()),
            Span::styled(
                format!("{}  ·  {}", state.current_model, state.current_provider),
                styles.header(),
            ),
            Span::styled(badge_text, styles.header()),
        ]),
        Line::from(vec![
            Span::styled(format!("  {session}"), styles.header()),
            Span::styled(
                format!(
                    "  ·  {} key(s)  ·  theme: {}",
                    state.key_status.len(),
                    state.theme.name()
                ),
                styles.header(),
            ),
        ]),
        Line::from(Span::styled(status, styles.header())),
    ])
    .style(styles.header());
    frame.render_widget(header, area);
}

fn render_chat(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let styles = state.theme.styles();
    let has_started_work = state.session_id.is_some()
        || state.messages.iter().any(|message| {
            matches!(
                message.role.as_str(),
                "user" | "assistant" | "tool_call" | "tool_result" | "tool" | "shell"
            )
        });
    if !state.busy && !has_started_work {
        render_welcome(frame, state, area, &styles);
        return;
    }

    let mut lines: Vec<Line> = Vec::new();
    if state.messages.is_empty() {
        lines.push(Line::from(Span::styled(
            "Welcome! Type a message and press Enter to send it. /help lists the commands.",
            styles.dim(),
        )));
        lines.push(Line::from(""));
    }
    for message in &state.messages {
        let role_style = styles.role_style(&message.role);
        // Subagent output is indented under the parent conversation.
        let indent = if message.role == "subagent" { "  " } else { "" };
        for (index, chunk) in message.content.split('\n').enumerate() {
            if index == 0 {
                lines.push(Line::from(vec![
                    Span::styled(format!("[{}] ", message.timestamp), styles.dim()),
                    Span::styled(format!("{}: ", message.role), role_style),
                    Span::styled(format!("{indent}{chunk}"), styles.body()),
                ]));
            } else {
                lines.push(Line::from(Span::styled(
                    format!("{indent}{chunk}"),
                    styles.body(),
                )));
            }
        }
        lines.push(Line::from(""));
    }
    let text = Text::from(lines);

    // Follow the bottom of the conversation unless the user scrolled up.
    let total_lines = Paragraph::new(text.clone())
        .wrap(Wrap { trim: false })
        .line_count(area.width);
    let max_scroll = total_lines
        .saturating_sub(area.height as usize)
        .min(u16::MAX as usize) as u16;
    state.max_scroll = max_scroll;
    if state.stick_to_bottom {
        state.scroll_offset = max_scroll;
    } else {
        state.scroll_offset = state.scroll_offset.min(max_scroll);
    }

    let chat = Paragraph::new(text)
        .style(styles.base())
        .wrap(Wrap { trim: false })
        .scroll((state.scroll_offset, 0));
    frame.render_widget(chat, area);

    if max_scroll > 0 {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"))
            .track_symbol(Some("│"))
            .thumb_symbol("█")
            .style(styles.border());
        let mut scrollbar_state =
            ScrollbarState::new(max_scroll as usize).position(state.scroll_offset as usize);
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }

    if !state.stick_to_bottom && state.scroll_offset < max_scroll {
        let remaining = max_scroll.saturating_sub(state.scroll_offset);
        let indicator_text = format!(" ▲ Scrolled up ({remaining} lines below · End to resume) ");
        let width = (indicator_text.len() as u16).min(area.width.saturating_sub(2));
        let indicator_area = Rect {
            x: area.x + area.width.saturating_sub(width + 2),
            y: area.y + area.height.saturating_sub(1),
            width,
            height: 1,
        };
        let indicator = Paragraph::new(Span::styled(
            indicator_text,
            Style::default()
                .bg(styles.warning_color)
                .fg(styles.bg)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ));
        frame.render_widget(indicator, indicator_area);
    }
}

fn render_welcome(
    frame: &mut Frame,
    state: &TuiState,
    area: Rect,
    styles: &crate::theme::ThemeStyles,
) {
    let mut lines = vec![
        Line::from(Span::styled(
            "  ███████╗██╗ ██████╗██╗  ██╗",
            Style::default()
                .fg(styles.assistant_color)
                .add_modifier(ratatui::style::Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "  ██╔════╝██║██╔════╝██║ ██╔╝",
            Style::default().fg(styles.assistant_color),
        )),
        Line::from(Span::styled(
            "  ███████╗██║██║     █████╔╝ ",
            Style::default().fg(styles.assistant_color),
        )),
        Line::from(Span::styled(
            "  ╚════██║██║██║     ██╔═██╗ ",
            Style::default().fg(styles.assistant_color),
        )),
        Line::from(Span::styled(
            "  ███████║██║╚██████╗██║  ██╗",
            Style::default().fg(styles.assistant_color),
        )),
        Line::from(Span::styled(
            "  ╚══════╝╚═╝ ╚═════╝╚═╝  ╚═╝",
            Style::default().fg(styles.assistant_color),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "A I   C O D I N G   A G E N T",
            Style::default()
                .fg(styles.metadata_color)
                .add_modifier(ratatui::style::Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "What are we building today?",
            Style::default()
                .fg(styles.fg)
                .add_modifier(ratatui::style::Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "Type a task to begin  ·  /help for commands",
            styles.dim(),
        )),
    ];
    for message in &state.messages {
        if message.role == "system" || message.role == "error" {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                message.content.as_str(),
                styles.role_style(&message.role),
            )));
        }
    }

    let height = (lines.len() as u16).min(area.height);
    let splash_area = Rect {
        x: area.x,
        y: area.y + area.height.saturating_sub(height) / 3,
        width: area.width,
        height,
    };
    let splash = Paragraph::new(Text::from(lines))
        .alignment(Alignment::Center)
        .style(styles.base())
        .wrap(Wrap { trim: false });
    frame.render_widget(splash, splash_area);
}

fn render_palette(frame: &mut Frame, state: &TuiState, area: Rect) {
    let styles = state.theme.styles();
    let matches = state.matching_commands();
    let selected = state.command_selection.min(matches.len().saturating_sub(1));
    let items: Vec<ListItem> = matches
        .iter()
        .enumerate()
        .map(|(index, command)| {
            let marker = if index == selected { "▶" } else { " " };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {marker} {command:<10}"),
                    Style::default().fg(styles.tool_color),
                ),
                Span::styled(
                    format!(" {}", command_description(command)),
                    styles.role_style("system"),
                ),
            ]))
        })
        .collect();
    let palette = List::new(items)
        .style(styles.base())
        .highlight_style(styles.selected())
        .block(
            Block::bordered()
                .style(styles.base())
                .title(format!(" commands ({}) — ↑/↓ · Enter ", matches.len())),
        );
    let mut list_state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(palette, area, &mut list_state);
}

fn render_input(frame: &mut Frame, state: &TuiState, textarea: &mut TextArea<'static>, area: Rect) {
    let styles = state.theme.styles();
    let (title, border) = match state.input_mode {
        InputMode::Command => (
            " Command — Enter runs · Esc cancels ",
            Style::default().fg(styles.tool_color),
        ),
        InputMode::Normal => (
            " Input — Enter sends · Ctrl+Enter newline · Wheel/Shift+↑↓ scroll · Ctrl+R history ",
            styles.border(),
        ),
    };
    textarea.set_block(
        Block::bordered()
            .title(title)
            .border_style(border)
            .style(styles.base()),
    );
    frame.render_widget(&*textarea, area);
}

fn render_history_overlay(frame: &mut Frame, state: &TuiState, area: Rect) {
    let styles = state.theme.styles();
    let popup = centered_rect(70, 60, area);
    frame.render_widget(Clear, popup);

    let items: Vec<ListItem> = state
        .history
        .iter()
        .map(|entry| ListItem::new(Line::from(format!(" {entry} "))))
        .collect();
    let list = List::new(items)
        .style(styles.base())
        .highlight_style(styles.selected())
        .block(
            Block::bordered()
                .style(styles.base())
                .title(" history (Up/Down · Enter select · Esc close) "),
        );
    let mut list_state = ListState::default().with_selected(Some(state.history_selection));
    frame.render_stateful_widget(list, popup, &mut list_state);
}

/// API-key setup wizard overlay: provider picker → paste → validating.
fn render_key_setup(frame: &mut Frame, state: &TuiState, area: Rect) {
    let Some(setup) = &state.key_setup else {
        return;
    };
    let styles = state.theme.styles();
    let popup = centered_rect(60, 50, area);
    frame.render_widget(Clear, popup);

    match setup.stage {
        SetupStage::ChooseProvider => {
            let items: Vec<ListItem> = SETUP_PROVIDERS
                .iter()
                .map(|(id, label)| {
                    ListItem::new(Line::from(vec![
                        Span::styled(format!(" {label:<20} "), styles.body()),
                        Span::styled(format!("({id})"), styles.dim()),
                    ]))
                })
                .collect();
            let list = List::new(items)
                .style(styles.base())
                .highlight_style(styles.selected())
                .block(
                    Block::bordered()
                        .style(styles.base())
                        .title(" key setup — choose a provider ")
                        .title_bottom(
                            Line::from(" ↑/↓ select · Enter continue · Esc skip ").right_aligned(),
                        ),
                );
            let mut list_state = ListState::default().with_selected(Some(setup.selection));
            frame.render_stateful_widget(list, popup, &mut list_state);
        }
        SetupStage::EnterKey => {
            let mut lines = vec![
                Line::from(Span::styled(
                    format!(
                        " Provider: {} ({})",
                        setup.provider_label(),
                        setup.provider()
                    ),
                    styles.body(),
                )),
                Line::from(""),
                Line::from(Span::styled(" Paste your API key (hidden):", styles.body())),
                Line::from(Span::styled(
                    format!(" {}", setup.masked_key()),
                    styles.role_style("user"),
                )),
            ];
            if let Some(status) = &setup.status {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!(" {status}"),
                    styles.role_style("error"),
                )));
            }
            let paragraph = Paragraph::new(Text::from(lines))
                .style(styles.base())
                .wrap(Wrap { trim: false })
                .block(
                    Block::bordered()
                        .style(styles.base())
                        .title(" key setup — paste key ")
                        .title_bottom(Line::from(" Enter save · Esc back ").right_aligned()),
                );
            frame.render_widget(paragraph, popup);
        }
        SetupStage::EnterAccountId => {
            let mut lines = vec![
                Line::from(Span::styled(
                    " Cloudflare Workers AI requires your account ID (not the API token ID).",
                    styles.body(),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    " Find it in the Cloudflare dashboard on the Workers AI overview page.",
                    styles.dim(),
                )),
                Line::from(""),
                Line::from(Span::styled(" Account ID:", styles.body())),
                Line::from(Span::styled(
                    format!(" {}", setup.account_id_input),
                    styles.role_style("user"),
                )),
            ];
            if let Some(status) = &setup.status {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!(" {status}"),
                    styles.role_style("error"),
                )));
            }
            let paragraph = Paragraph::new(Text::from(lines))
                .style(styles.base())
                .wrap(Wrap { trim: false })
                .block(
                    Block::bordered()
                        .style(styles.base())
                        .title(" key setup — Cloudflare account ")
                        .title_bottom(Line::from(" Enter continue · Esc back ").right_aligned()),
                );
            frame.render_widget(paragraph, popup);
        }
        SetupStage::Validating => {
            let paragraph = Paragraph::new(Text::from(vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!(" Validating the {} key…", setup.provider_label()),
                    styles.body(),
                )),
            ]))
            .style(styles.base())
            .block(Block::bordered().style(styles.base()).title(" key setup "));
            frame.render_widget(paragraph, popup);
        }
    }
}

/// Plan-review overlay (Step 4): approve · edit · cancel.
fn render_plan_review(state: &TuiState, frame: &mut Frame, area: Rect) {
    let styles = state.theme.styles();
    let popup = centered_rect(60, 30, area);
    frame.render_widget(Clear, popup);

    let paragraph = Paragraph::new(Text::from(vec![
        Line::from(""),
        Line::from(Span::styled(
            " A plan is ready — full text above in the chat.",
            styles.body(),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  [a]", styles.selected()),
            Span::styled("pprove    ", styles.body()),
            Span::styled("[e]", styles.selected()),
            Span::styled("dit    ", styles.body()),
            Span::styled("[c]", styles.selected()),
            Span::styled("ancel", styles.body()),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            " approve executes it · edit lets you refine it · cancel discards it",
            styles.dim(),
        )),
    ]))
    .style(styles.base())
    .block(
        Block::bordered()
            .style(styles.base())
            .title(" plan review ")
            .title_bottom(Line::from(" Esc cancels ").right_aligned()),
    );
    frame.render_widget(paragraph, popup);
}

/// Interactive `/models` picker: every model the configured providers offer, with live search.
fn render_models_picker(frame: &mut Frame, state: &TuiState, area: Rect) {
    let Some(picker) = &state.models_picker else {
        return;
    };
    let styles = state.theme.styles();
    let popup = centered_rect(70, 60, area);
    frame.render_widget(Clear, popup);

    let filtered = picker.filtered_options();
    let mut title = if picker.filter.is_empty() {
        format!(" models ({}) ", picker.options.len())
    } else {
        format!(
            " models ({}/{}) — Search: \"{}\" ",
            filtered.len(),
            picker.options.len(),
            picker.filter
        )
    };
    if picker.loading {
        title.push_str("· fetching live lists… ");
    }
    let bottom = match &picker.status {
        Some(status) => format!(" {status} · type search · ↑/↓ · Enter · Esc "),
        None => " ↑/↓ choose · type to search · Backspace · Enter switch · Esc close ".to_string(),
    };

    if picker.options.is_empty() {
        let paragraph = Paragraph::new(Text::from(vec![
            Line::from(""),
            Line::from(Span::styled(
                " No models found for the configured providers.",
                styles.body(),
            )),
            Line::from(""),
            Line::from(Span::styled(
                " Set any id directly with /model [provider:]model.",
                styles.dim(),
            )),
        ]))
        .style(styles.base())
        .wrap(Wrap { trim: false })
        .block(
            Block::bordered()
                .style(styles.base())
                .title(title)
                .title_bottom(Line::from(bottom).right_aligned()),
        );
        frame.render_widget(paragraph, popup);
        return;
    }

    if filtered.is_empty() {
        let paragraph = Paragraph::new(Text::from(vec![
            Line::from(""),
            Line::from(Span::styled(
                format!(" No models match search query: \"{}\"", picker.filter),
                styles.body(),
            )),
            Line::from(""),
            Line::from(Span::styled(
                " Press Backspace to clear, or Esc to close.",
                styles.dim(),
            )),
        ]))
        .style(styles.base())
        .wrap(Wrap { trim: false })
        .block(
            Block::bordered()
                .style(styles.base())
                .title(title)
                .title_bottom(Line::from(bottom).right_aligned()),
        );
        frame.render_widget(paragraph, popup);
        return;
    }

    let current_provider = normalize_provider(&state.current_provider);
    let selected = picker.selection.min(filtered.len().saturating_sub(1));
    let items: Vec<ListItem> = filtered
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let marker = if index == selected { "▶" } else { " " };
            let mut spans = vec![
                Span::styled(format!(" {marker} {:<9} ", option.provider), styles.dim()),
                Span::styled(option.model.clone(), styles.body()),
            ];
            if option.provider == current_provider && option.model == state.current_model {
                spans.push(Span::styled("  ← current", styles.role_style("system")));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .style(styles.base())
        .highlight_style(styles.selected())
        .block(
            Block::bordered()
                .style(styles.base())
                .title(title)
                .title_bottom(Line::from(bottom).right_aligned()),
        );
    let mut list_state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(list, popup, &mut list_state);
}

/// Interactive `/themes` picker: preview and select themes with search.
/// Interactive `/themes` picker: preview and select themes, color wheel studio, and JSON import.
fn render_themes_picker(frame: &mut Frame, state: &TuiState, area: Rect) {
    let Some(picker) = &state.themes_picker else {
        return;
    };

    match &picker.mode {
        ThemePickerMode::ColorWheel(wheel) => {
            render_color_wheel(frame, state, area, picker, wheel);
            return;
        }
        ThemePickerMode::JsonImport(import) => {
            render_json_import(frame, state, area, picker, import);
            return;
        }
        ThemePickerMode::Browse => {}
    }

    let styles = state.theme.styles();
    let popup = centered_rect(78, 80, area);
    frame.render_widget(Clear, popup);

    let filtered = picker.filtered_items();
    let title = if picker.filter.is_empty() {
        format!(" themes ({}) ", picker.options.len())
    } else {
        format!(
            " themes ({}/{}) — Search: \"{}\" ",
            filtered.len(),
            picker.options.len(),
            picker.filter
        )
    };

    if filtered.is_empty() {
        let paragraph = Paragraph::new(Text::from(vec![
            Line::from(""),
            Line::from(Span::styled(
                format!(" No themes match search query: \"{}\"", picker.filter),
                styles.body(),
            )),
            Line::from(""),
            Line::from(Span::styled(
                " Press Backspace to clear filter, or Esc to close.",
                styles.dim(),
            )),
        ]))
        .style(styles.base())
        .wrap(Wrap { trim: false })
        .block(
            Block::bordered()
                .style(styles.base())
                .title(title)
                .title_bottom(Line::from(" Backspace clear · Esc close ").right_aligned()),
        );
        frame.render_widget(paragraph, popup);
        return;
    }

    let selected = picker.selection.min(filtered.len().saturating_sub(1));

    // Split popup into theme list (top) and theme preview card (bottom)
    let chunks = Layout::vertical([Constraint::Min(8), Constraint::Length(9)]).split(popup);

    let list_area = chunks[0];
    let preview_area = chunks[1];

    let items: Vec<ListItem> = filtered
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let is_sel = index == selected;
            let marker = if is_sel { "▶ " } else { "  " };

            match item {
                ThemePickerItem::CreateCustomWheel => {
                    let spans = vec![
                        Span::styled(
                            marker,
                            if is_sel {
                                styles.selected()
                            } else {
                                styles.dim()
                            },
                        ),
                        Span::styled(
                            "✦ [Create Custom Theme with Color Wheel]",
                            if is_sel {
                                styles.selected()
                            } else {
                                styles.role_style("system").add_modifier(Modifier::BOLD)
                            },
                        ),
                        Span::styled("  ← interactive 360° wheel studio", styles.dim()),
                    ];
                    ListItem::new(Line::from(spans))
                }
                ThemePickerItem::ImportCustomJson => {
                    let spans = vec![
                        Span::styled(
                            marker,
                            if is_sel {
                                styles.selected()
                            } else {
                                styles.dim()
                            },
                        ),
                        Span::styled(
                            "📋 [Import Custom Theme from JSON]",
                            if is_sel {
                                styles.selected()
                            } else {
                                styles.role_style("metadata").add_modifier(Modifier::BOLD)
                            },
                        ),
                        Span::styled("  ← paste or type theme code", styles.dim()),
                    ];
                    ListItem::new(Line::from(spans))
                }
                ThemePickerItem::CustomTheme(theme) => {
                    let is_active = theme.name() == state.theme.name();
                    let mut spans = vec![
                        Span::styled(
                            marker,
                            if is_sel {
                                styles.selected()
                            } else {
                                styles.dim()
                            },
                        ),
                        Span::styled(
                            format!("{:<22} ", theme.display_name()),
                            styles.body().add_modifier(Modifier::BOLD),
                        ),
                    ];
                    let tstyles = theme.styles();
                    spans.push(Span::styled("■ ", Style::default().fg(tstyles.bg)));
                    spans.push(Span::styled("■ ", Style::default().fg(tstyles.fg)));
                    spans.push(Span::styled(
                        "■ ",
                        Style::default().fg(tstyles.assistant_color),
                    ));
                    spans.push(Span::styled(
                        "■ ",
                        Style::default().fg(tstyles.border_color),
                    ));
                    spans.push(Span::styled(
                        "■ ",
                        Style::default().fg(tstyles.success_color),
                    ));
                    spans.push(Span::styled(
                        "■ ",
                        Style::default().fg(tstyles.warning_color),
                    ));
                    spans.push(Span::styled("■ ", Style::default().fg(tstyles.error_color)));
                    spans.push(Span::styled(
                        "■  ",
                        Style::default().fg(tstyles.metadata_color),
                    ));
                    spans.push(Span::styled("[Custom] ", styles.role_style("warning")));
                    if is_active {
                        spans.push(Span::styled("← active", styles.role_style("system")));
                    }
                    ListItem::new(Line::from(spans))
                }
                ThemePickerItem::BuiltinTheme(theme) => {
                    let is_active = theme.name() == state.theme.name();
                    let mut spans = vec![
                        Span::styled(
                            marker,
                            if is_sel {
                                styles.selected()
                            } else {
                                styles.dim()
                            },
                        ),
                        Span::styled(format!("{:<22} ", theme.display_name()), styles.body()),
                    ];
                    let tstyles = theme.styles();
                    spans.push(Span::styled("■ ", Style::default().fg(tstyles.bg)));
                    spans.push(Span::styled("■ ", Style::default().fg(tstyles.fg)));
                    spans.push(Span::styled(
                        "■ ",
                        Style::default().fg(tstyles.assistant_color),
                    ));
                    spans.push(Span::styled(
                        "■ ",
                        Style::default().fg(tstyles.border_color),
                    ));
                    spans.push(Span::styled(
                        "■ ",
                        Style::default().fg(tstyles.success_color),
                    ));
                    spans.push(Span::styled(
                        "■ ",
                        Style::default().fg(tstyles.warning_color),
                    ));
                    spans.push(Span::styled("■ ", Style::default().fg(tstyles.error_color)));
                    spans.push(Span::styled(
                        "■  ",
                        Style::default().fg(tstyles.metadata_color),
                    ));
                    if is_active {
                        spans.push(Span::styled("← active", styles.role_style("system")));
                    }
                    ListItem::new(Line::from(spans))
                }
            }
        })
        .collect();

    let list = List::new(items)
        .style(styles.base())
        .highlight_style(styles.selected())
        .block(
            Block::bordered()
                .style(styles.base())
                .title(title)
                .title_bottom(
                    Line::from(" ↑/↓ browse · type to search · Enter select · Esc close ")
                        .right_aligned(),
                ),
        );
    let mut list_state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(list, list_area, &mut list_state);

    // Render Preview card for the highlighted item
    if let Some(highlighted) = filtered.get(selected) {
        match highlighted {
            ThemePickerItem::CreateCustomWheel => {
                let preview_lines = vec![
                    Line::from(vec![Span::styled(
                        " ✦ Visual Color Wheel Studio ",
                        Style::default()
                            .bg(styles.assistant_color)
                            .fg(styles.bg)
                            .add_modifier(Modifier::BOLD),
                    )]),
                    Line::from(""),
                    Line::from(vec![Span::styled(
                        " Design your custom palette with an interactive 360° truecolor hue wheel, ",
                        styles.body(),
                    )]),
                    Line::from(vec![Span::styled(
                        " brightness & saturation sliders, and live chat preview. ",
                        styles.body(),
                    )]),
                    Line::from(vec![Span::styled(
                        " Press Enter to launch Studio.",
                        styles.role_style("system").add_modifier(Modifier::BOLD),
                    )]),
                ];
                let preview = Paragraph::new(Text::from(preview_lines))
                    .style(styles.base())
                    .wrap(Wrap { trim: false })
                    .block(
                        Block::bordered()
                            .style(styles.base())
                            .title(" Custom Theme Builder "),
                    );
                frame.render_widget(preview, preview_area);
            }
            ThemePickerItem::ImportCustomJson => {
                let preview_lines = vec![
                    Line::from(vec![Span::styled(
                        " 📋 JSON Theme Importer ",
                        Style::default()
                            .bg(styles.metadata_color)
                            .fg(styles.bg)
                            .add_modifier(Modifier::BOLD),
                    )]),
                    Line::from(""),
                    Line::from(vec![Span::styled(
                        " Import or paste JSON containing your theme name and the 8 hex colors: ",
                        styles.body(),
                    )]),
                    Line::from(vec![Span::styled(
                        " background, primary, accent, secondary, success, warning, error, metadata. ",
                        styles.dim(),
                    )]),
                    Line::from(vec![Span::styled(
                        " Press Enter to launch Importer.",
                        styles.role_style("system").add_modifier(Modifier::BOLD),
                    )]),
                ];
                let preview = Paragraph::new(Text::from(preview_lines))
                    .style(styles.base())
                    .wrap(Wrap { trim: false })
                    .block(
                        Block::bordered()
                            .style(styles.base())
                            .title(" JSON Importer "),
                    );
                frame.render_widget(preview, preview_area);
            }
            ThemePickerItem::CustomTheme(theme) | ThemePickerItem::BuiltinTheme(theme) => {
                let tstyles = theme.styles();
                let phrases_joined = theme.phrases().join(" · ");
                let preview_lines = vec![
                    Line::from(vec![
                        Span::styled(
                            format!(" {} ", theme.display_name()),
                            Style::default()
                                .bg(tstyles.assistant_color)
                                .fg(tstyles.bg)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(format!("  id: {}", theme.name()), styles.dim()),
                    ]),
                    Line::from(vec![
                        Span::styled(" Vibe: ", styles.dim()),
                        Span::styled(theme.vibe(), styles.body()),
                    ]),
                    Line::from(vec![
                        Span::styled(" Phrases: ", styles.dim()),
                        Span::styled(phrases_joined, styles.role_style("system")),
                    ]),
                    Line::from(vec![
                        Span::styled(" Palette: ", styles.dim()),
                        Span::styled(" bg ", Style::default().bg(tstyles.bg).fg(tstyles.fg)),
                        Span::raw(" "),
                        Span::styled(" fg ", Style::default().bg(tstyles.fg).fg(tstyles.bg)),
                        Span::raw(" "),
                        Span::styled(
                            " accent ",
                            Style::default().bg(tstyles.assistant_color).fg(tstyles.bg),
                        ),
                        Span::raw(" "),
                        Span::styled(
                            " sec ",
                            Style::default().bg(tstyles.border_color).fg(tstyles.bg),
                        ),
                        Span::raw(" "),
                        Span::styled(
                            " ok ",
                            Style::default().bg(tstyles.success_color).fg(tstyles.bg),
                        ),
                        Span::raw(" "),
                        Span::styled(
                            " warn ",
                            Style::default().bg(tstyles.warning_color).fg(tstyles.bg),
                        ),
                        Span::raw(" "),
                        Span::styled(
                            " err ",
                            Style::default().bg(tstyles.error_color).fg(tstyles.fg),
                        ),
                        Span::raw(" "),
                        Span::styled(
                            " meta ",
                            Style::default().bg(tstyles.metadata_color).fg(tstyles.bg),
                        ),
                    ]),
                ];
                let preview = Paragraph::new(Text::from(preview_lines))
                    .style(styles.base())
                    .wrap(Wrap { trim: false })
                    .block(
                        Block::bordered()
                            .style(styles.base())
                            .title(" Theme Details & Preview "),
                    );
                frame.render_widget(preview, preview_area);
            }
        }
    }
}

/// Visual Color Wheel & Palette Studio.
fn render_color_wheel(
    frame: &mut Frame,
    state: &TuiState,
    area: Rect,
    _picker: &ThemesPicker,
    wheel: &ColorWheelState,
) {
    let styles = state.theme.styles();
    let popup = centered_rect(88, 88, area);
    frame.render_widget(Clear, popup);

    let main_block = Block::bordered()
        .style(styles.base())
        .title(" ✦ Custom Theme Studio — Interactive Color Wheel ")
        .title_bottom(
            Line::from(" Tab/Down next · Shift+Tab/Up prev · 1-8 jump · ←/→ Hue · [/] Brightness · Enter/Ctrl+S save · Esc back ")
                .right_aligned(),
        );
    let inner = main_block.inner(popup);
    frame.render_widget(main_block, popup);

    let chunks = Layout::horizontal([Constraint::Length(38), Constraint::Min(40)]).split(inner);
    let left_area = chunks[0];
    let right_area = chunks[1];

    // Left Column: Name + 8 Roles + Save Button + Status Message
    let left_chunks = Layout::vertical([
        Constraint::Length(3), // Theme Name
        Constraint::Min(10),   // 8 Color Roles
        Constraint::Length(3), // Save Button
        Constraint::Length(2), // Optional message
    ])
    .split(left_area);

    // 0: Theme Name
    let name_focus = wheel.active_field == 0;
    let name_border_style = if name_focus {
        styles.selected()
    } else {
        styles.dim()
    };
    let name_block = Block::bordered()
        .style(styles.base())
        .border_style(name_border_style)
        .title(if name_focus {
            " ▶ Theme Name "
        } else {
            " Theme Name "
        });
    let name_para = Paragraph::new(Line::from(vec![
        Span::styled(
            if wheel.name.is_empty() {
                "type theme name..."
            } else {
                &wheel.name
            },
            if wheel.name.is_empty() {
                styles.dim()
            } else {
                styles.body().add_modifier(Modifier::BOLD)
            },
        ),
        if name_focus {
            Span::styled("█", styles.selected())
        } else {
            Span::raw("")
        },
    ]))
    .block(name_block);
    frame.render_widget(name_para, left_chunks[0]);

    // 1..=8: Color Roles List
    let role_items: Vec<ListItem> = ColorWheelState::ROLE_LABELS
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let field_num = i + 1;
            let is_focused = wheel.active_field == field_num;
            let marker = if is_focused { "▶ " } else { "  " };
            let hex = &wheel.colors[i];
            let parsed_color = Theme::parse_hex_color(hex).unwrap_or(Color::White);

            let spans = vec![
                Span::styled(
                    marker,
                    if is_focused {
                        styles.selected()
                    } else {
                        styles.dim()
                    },
                ),
                Span::styled(
                    format!("{:<18} ", label),
                    if is_focused {
                        styles.body().add_modifier(Modifier::BOLD)
                    } else {
                        styles.body()
                    },
                ),
                Span::styled("██ ", Style::default().fg(parsed_color)),
                Span::styled(
                    hex.clone(),
                    if is_focused {
                        styles.role_style("system")
                    } else {
                        styles.dim()
                    },
                ),
            ];
            ListItem::new(Line::from(spans))
        })
        .collect();

    let roles_block = Block::bordered()
        .style(styles.base())
        .title(" Palette Roles (1-8) ");
    let roles_list = List::new(role_items).block(roles_block);
    frame.render_widget(roles_list, left_chunks[1]);

    // 9: Save & Apply Button
    let save_focus = wheel.active_field == 9;
    let save_block = Block::bordered()
        .style(styles.base())
        .border_style(if save_focus {
            styles.selected()
        } else {
            styles.dim()
        });
    let save_text = if save_focus {
        Line::from(vec![
            Span::styled(" ▶ ", styles.selected()),
            Span::styled(
                "[ Save & Apply Theme ]",
                Style::default()
                    .bg(styles.assistant_color)
                    .fg(styles.bg)
                    .add_modifier(Modifier::BOLD),
            ),
        ])
    } else {
        Line::from(vec![
            Span::raw("   "),
            Span::styled("[ Save & Apply Theme ]", styles.role_style("system")),
        ])
    };
    let save_para = Paragraph::new(save_text).block(save_block);
    frame.render_widget(save_para, left_chunks[2]);

    // Optional error/status message
    if let Some((msg, is_err)) = &wheel.message {
        let style = if *is_err {
            styles.role_style("error")
        } else {
            styles.role_style("shell")
        };
        let msg_para = Paragraph::new(Line::from(Span::styled(format!(" {msg}"), style)));
        frame.render_widget(msg_para, left_chunks[3]);
    }

    // Right Column: Color Wheel Studio & Live Component Preview
    let right_chunks = Layout::vertical([
        Constraint::Length(12), // Wheel & Sliders Studio
        Constraint::Min(8),     // Live Component Preview
    ])
    .split(right_area);

    // Studio box
    let active_role_idx = if wheel.active_field >= 1 && wheel.active_field <= 8 {
        wheel.active_field - 1
    } else {
        2 // default to accent
    };
    let active_label = ColorWheelState::ROLE_LABELS[active_role_idx];
    let active_hex = &wheel.colors[active_role_idx];
    let active_color = Theme::parse_hex_color(active_hex).unwrap_or(Color::White);

    // Build 24-step rainbow hue spectrum
    let mut hue_spans = vec![Span::raw("   ")];
    for step in 0..24 {
        let h = (step as f32 / 24.0) * 360.0;
        let (r, g, b) = Theme::hsl_to_rgb(h, 1.0, 0.5);
        hue_spans.push(Span::styled("█", Style::default().fg(Color::Rgb(r, g, b))));
    }
    hue_spans.push(Span::styled(
        format!("  {:.0}°", wheel.hue),
        styles.body().add_modifier(Modifier::BOLD),
    ));

    let cursor_pos = ((wheel.hue / 360.0) * 24.0).floor() as usize;
    let mut cursor_spans = vec![Span::raw("   ")];
    for i in 0..24 {
        if i == cursor_pos.min(23) {
            cursor_spans.push(Span::styled(
                "▲",
                Style::default()
                    .fg(styles.assistant_color)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            cursor_spans.push(Span::raw(" "));
        }
    }
    cursor_spans.push(Span::styled("  (←/→ rotate)", styles.dim()));

    // Lightness slider
    let l_pct = (wheel.lightness * 100.0).round() as usize;
    let l_pos = ((wheel.lightness - 0.05) / 0.90 * 20.0)
        .round()
        .clamp(0.0, 19.0) as usize;
    let mut l_bar = String::new();
    for i in 0..20 {
        if i == l_pos {
            l_bar.push('●');
        } else {
            l_bar.push('─');
        }
    }

    // Saturation slider
    let s_pct = (wheel.saturation * 100.0).round() as usize;
    let s_pos = (wheel.saturation * 20.0).round().clamp(0.0, 19.0) as usize;
    let mut s_bar = String::new();
    for i in 0..20 {
        if i == s_pos {
            s_bar.push('●');
        } else {
            s_bar.push('─');
        }
    }

    let studio_lines = vec![
        Line::from(vec![
            Span::styled(" Editing Role: ", styles.dim()),
            Span::styled(active_label, styles.body().add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled("██████", Style::default().fg(active_color)),
            Span::styled(
                format!("  {}  ", active_hex),
                Style::default()
                    .bg(active_color)
                    .fg(if wheel.lightness > 0.5 {
                        Color::Black
                    } else {
                        Color::White
                    }),
            ),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            " Color Wheel (Hue):",
            styles.role_style("system"),
        )]),
        Line::from(hue_spans),
        Line::from(cursor_spans),
        Line::from(vec![
            Span::styled(" Brightness:  ", styles.dim()),
            Span::styled(format!("[{l_bar}] {l_pct}%  ([/] adjust)"), styles.body()),
        ]),
        Line::from(vec![
            Span::styled(" Saturation:  ", styles.dim()),
            Span::styled(format!("[{s_bar}] {s_pct}%  ({{/}} adjust)"), styles.body()),
        ]),
    ];

    let studio_block = Block::bordered()
        .style(styles.base())
        .title(" Interactive Color Wheel Studio ");
    let studio_para = Paragraph::new(Text::from(studio_lines)).block(studio_block);
    frame.render_widget(studio_para, right_chunks[0]);

    // Live Component Preview Box using current 8 designed colors
    let prev_bg = Theme::parse_hex_color(&wheel.colors[0]).unwrap_or(Color::Black);
    let prev_fg = Theme::parse_hex_color(&wheel.colors[1]).unwrap_or(Color::White);
    let prev_accent = Theme::parse_hex_color(&wheel.colors[2]).unwrap_or(Color::Cyan);
    let prev_sec = Theme::parse_hex_color(&wheel.colors[3]).unwrap_or(Color::DarkGray);
    let prev_ok = Theme::parse_hex_color(&wheel.colors[4]).unwrap_or(Color::Green);
    let prev_warn = Theme::parse_hex_color(&wheel.colors[5]).unwrap_or(Color::Yellow);
    let prev_err = Theme::parse_hex_color(&wheel.colors[6]).unwrap_or(Color::Red);
    let prev_meta = Theme::parse_hex_color(&wheel.colors[7]).unwrap_or(Color::Blue);

    let preview_lines = vec![
        Line::from(vec![
            Span::styled(
                " User: ",
                Style::default()
                    .fg(prev_accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Show me the live theme preview!",
                Style::default().fg(prev_fg),
            ),
        ]),
        Line::from(vec![
            Span::styled(" Assistant: ", Style::default().fg(prev_accent)),
            Span::styled(
                "All 8 colors update in real time on this canvas.",
                Style::default().fg(prev_fg),
            ),
        ]),
        Line::from(vec![
            Span::styled(" Tool [read_file]: ", Style::default().fg(prev_warn)),
            Span::styled(
                "crates/tui/src/theme.rs (32 lines)",
                Style::default().fg(prev_sec),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                " Shell: ",
                Style::default().fg(prev_ok).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "✓ All tests passed successfully",
                Style::default().fg(prev_ok),
            ),
        ]),
        Line::from(vec![
            Span::styled(" Error: ", Style::default().fg(prev_err)),
            Span::styled(
                "Sample error state illustration",
                Style::default().fg(prev_err),
            ),
            Span::styled("  ·  ", Style::default().fg(prev_sec)),
            Span::styled("12:00:00 [metadata]", Style::default().fg(prev_meta)),
        ]),
    ];

    let preview_block = Block::bordered()
        .style(Style::default().bg(prev_bg).fg(prev_fg))
        .border_style(Style::default().fg(prev_sec))
        .title(" Live Theme Component Preview ");
    let preview_para = Paragraph::new(Text::from(preview_lines))
        .block(preview_block)
        .style(Style::default().bg(prev_bg).fg(prev_fg));
    frame.render_widget(preview_para, right_chunks[1]);
}

/// JSON Theme Importer modal.
fn render_json_import(
    frame: &mut Frame,
    state: &TuiState,
    area: Rect,
    _picker: &ThemesPicker,
    import: &JsonImportState,
) {
    let styles = state.theme.styles();
    let popup = centered_rect(82, 82, area);
    frame.render_widget(Clear, popup);

    let main_block = Block::bordered()
        .style(styles.base())
        .title(" 📋 Import Custom Theme from JSON ")
        .title_bottom(
            Line::from(" Enter / Ctrl+S Save & Apply · Esc cancel/back ").right_aligned(),
        );
    let inner = main_block.inner(popup);
    frame.render_widget(main_block, popup);

    let chunks = Layout::vertical([
        Constraint::Length(2), // Instructions
        Constraint::Min(8),    // Buffer editor
        Constraint::Length(4), // Validation status
    ])
    .split(inner);

    let intro = Paragraph::new(Line::from(Span::styled(
        " Paste or edit theme JSON below with 'name' and the 8 colors (background, primary, accent, secondary, success, warning, error, metadata):",
        styles.dim(),
    )));
    frame.render_widget(intro, chunks[0]);

    // JSON Buffer
    let buffer_lines: Vec<Line> = import
        .buffer
        .lines()
        .map(|line| Line::from(Span::styled(line, styles.body())))
        .collect();
    let buffer_block = Block::bordered()
        .style(styles.base())
        .title(" JSON Payload ");
    let buffer_para = Paragraph::new(Text::from(buffer_lines))
        .block(buffer_block)
        .wrap(Wrap { trim: false });
    frame.render_widget(buffer_para, chunks[1]);

    // Status / Validation Bar
    let status_block = Block::bordered()
        .style(styles.base())
        .title(" Validation Status ");
    let status_line = match &import.status {
        Ok((name, theme)) => {
            let tstyles = theme.styles();
            Line::from(vec![
                Span::styled(" ✓ Valid JSON for theme '", styles.role_style("shell")),
                Span::styled(name, styles.body().add_modifier(Modifier::BOLD)),
                Span::styled("':  ", styles.role_style("shell")),
                Span::styled("■ ", Style::default().fg(tstyles.bg)),
                Span::styled("■ ", Style::default().fg(tstyles.fg)),
                Span::styled("■ ", Style::default().fg(tstyles.assistant_color)),
                Span::styled("■ ", Style::default().fg(tstyles.border_color)),
                Span::styled("■ ", Style::default().fg(tstyles.success_color)),
                Span::styled("■ ", Style::default().fg(tstyles.warning_color)),
                Span::styled("■ ", Style::default().fg(tstyles.error_color)),
                Span::styled("■  ", Style::default().fg(tstyles.metadata_color)),
                Span::styled(
                    "— Press Enter or Ctrl+S to Save & Apply!",
                    styles.selected(),
                ),
            ])
        }
        Err(err) => Line::from(vec![
            Span::styled(
                " ✗ ",
                styles.role_style("error").add_modifier(Modifier::BOLD),
            ),
            Span::styled(err, styles.role_style("error")),
        ]),
    };
    let status_para = Paragraph::new(status_line).block(status_block);
    frame.render_widget(status_para, chunks[2]);
}

/// Interactive `/skills` picker: explore and activate skills with search.
fn render_skills_picker(frame: &mut Frame, state: &TuiState, area: Rect) {
    let Some(picker) = &state.skills_picker else {
        return;
    };
    let styles = state.theme.styles();
    let popup = centered_rect(75, 65, area);
    frame.render_widget(Clear, popup);

    let filtered = picker.filtered_options();
    let title = if picker.filter.is_empty() {
        format!(" skills ({}) ", picker.options.len())
    } else {
        format!(
            " skills ({}/{}) — Search: \"{}\" ",
            filtered.len(),
            picker.options.len(),
            picker.filter
        )
    };

    if filtered.is_empty() {
        let paragraph = Paragraph::new(Text::from(vec![
            Line::from(""),
            Line::from(Span::styled(
                format!(" No skills match search query: \"{}\"", picker.filter),
                styles.body(),
            )),
            Line::from(""),
            Line::from(Span::styled(
                " Press Backspace to clear filter, or Esc to close.",
                styles.dim(),
            )),
        ]))
        .style(styles.base())
        .wrap(Wrap { trim: false })
        .block(
            Block::bordered()
                .style(styles.base())
                .title(title)
                .title_bottom(Line::from(" Backspace clear · Esc close ").right_aligned()),
        );
        frame.render_widget(paragraph, popup);
        return;
    }

    let selected = picker.selection.min(filtered.len().saturating_sub(1));
    let items: Vec<ListItem> = filtered
        .iter()
        .enumerate()
        .map(|(index, skill)| {
            let marker = if index == selected { "▶ " } else { "  " };
            ListItem::new(Line::from(vec![
                Span::styled(marker, styles.dim()),
                Span::styled(
                    format!("{:<18} ", skill.name),
                    Style::default()
                        .fg(styles.tool_color)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
                Span::styled(format!("— {} ", skill.description), styles.body()),
                Span::styled(format!("({})", skill.path.display()), styles.dim()),
            ]))
        })
        .collect();

    let list = List::new(items)
        .style(styles.base())
        .highlight_style(styles.selected())
        .block(
            Block::bordered()
                .style(styles.base())
                .title(title)
                .title_bottom(
                    Line::from(" ↑/↓ browse · type to search · Enter activate · Esc close ")
                        .right_aligned(),
                ),
        );
    let mut list_state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(list, popup, &mut list_state);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1])[1]
}
