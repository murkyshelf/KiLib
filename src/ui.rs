//! All rendering. Each screen is a `draw_*` function taking the whole [`App`].

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, Focus, Screen};
use crate::config::Field;

const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::DarkGray;

pub fn draw(frame: &mut Frame, app: &mut App) {
    match app.screen {
        Screen::Main => draw_main(frame, app),
        Screen::Settings => draw_settings(frame, app),
        Screen::Picker => draw_picker(frame, app),
        Screen::Diagnostics => draw_diagnostics(frame, app),
        Screen::Validate => draw_validate(frame, app),
        Screen::Search => draw_search(frame, app),
        Screen::Projects => draw_projects(frame, app),
        // The prompt sits over whatever the user was looking at, so the context
        // it interrupts stays visible behind it.
        Screen::Overwrite => {
            match app.behind {
                Screen::Search => draw_search(frame, app),
                _ => draw_main(frame, app),
            }
            draw_overwrite(frame, app);
        }
    }
}

// --------------------------------------------------------- overwrite prompt

fn draw_overwrite(frame: &mut Frame, app: &App) {
    let Some(pending) = app.pending.as_ref() else {
        return;
    };

    let width = 76u16.min(frame.area().width);
    let inner = width.saturating_sub(2).max(1);

    // Show as many of the doomed files as fit, and say how many are left.
    let room = frame.area().height.saturating_sub(12).max(1) as usize;
    let shown = pending.conflicts.len().min(room);

    let mut lines = vec![
        Line::from(Span::styled(
            format!("{} is already in the library.", pending.label()),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("Importing it replaces {} file(s):", pending.conflicts.len()),
            Style::default().fg(MUTED),
        )),
    ];
    for path in pending.conflicts.iter().take(shown) {
        lines.push(Line::from(Span::raw(format!(
            "  {}",
            fit(path, inner.saturating_sub(2))
        ))));
    }
    if pending.conflicts.len() > shown {
        lines.push(Line::from(Span::styled(
            format!("  … and {} more", pending.conflicts.len() - shown),
            Style::default().fg(MUTED),
        )));
    }
    lines.push(Line::from(""));

    // [A] and [S] only mean something while there is a queue behind this.
    let queued = app.run.as_ref().map(|r| r.total > 1).unwrap_or(false);
    let mut keys = vec![("Y", "Overwrite")];
    if queued {
        keys.push(("A", "Overwrite All"));
        keys.push(("S", "Skip"));
    }
    keys.push(("N", "Cancel"));

    if queued {
        if let Some(run) = app.run.as_ref() {
            lines.push(Line::from(Span::styled(
                format!("Job {} of {}.", run.done + 1, run.total),
                Style::default().fg(MUTED),
            )));
            lines.push(Line::from(""));
        }
    }
    lines.push(hints(&keys, inner));

    let height: usize = lines
        .iter()
        .map(|l| l.width().max(1).div_ceil(inner as usize))
        .sum();
    let area = centered(frame.area(), width, height as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Already In The Library ")
                    .border_style(Style::default().fg(Color::Yellow)),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// Middle-ellipsises `text` to at most `max` columns, keeping the tail.
fn fit(text: &str, max: u16) -> String {
    let max = max as usize;
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max || max < 6 {
        return text.to_string();
    }
    // Favour the tail: it holds the directory names that disambiguate paths.
    let tail = (max - 3) * 2 / 3;
    let head = max - 3 - tail;
    let head_part: String = chars[..head].iter().collect();
    let tail_part: String = chars[chars.len() - tail..].iter().collect();
    format!("{head_part}...{tail_part}")
}

const HINT_GAP: usize = 2;

/// The key bar, trimmed to fit.
///
/// When the bar is wider than the terminal, hints are dropped from the end
/// inwards but the last one is always kept — it is `[Q] Quit` or `[Esc] Back`,
/// the one a stuck user needs most.
pub(crate) fn hints(items: &[(&str, &str)], width: u16) -> Line<'static> {
    if items.is_empty() {
        return Line::from("");
    }
    let cost = |(key, action): &(&str, &str)| key.chars().count() + action.chars().count() + 3;

    let last = items.len() - 1;
    let mut keep = vec![true; items.len()];
    let mut total: usize = items.iter().map(cost).sum::<usize>() + HINT_GAP * last;
    let mut i = last;
    while total > width as usize && i > 0 {
        i -= 1;
        keep[i] = false;
        total -= cost(&items[i]) + HINT_GAP;
    }

    let mut spans = Vec::new();
    for (item, _) in items.iter().zip(keep).filter(|(_, keep)| *keep) {
        if !spans.is_empty() {
            spans.push(Span::raw(" ".repeat(HINT_GAP)));
        }
        spans.push(Span::styled(
            format!("[{}]", item.0),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(format!(" {}", item.1)));
    }
    Line::from(spans)
}

// --------------------------------------------------------------- main screen

fn draw_main(frame: &mut Frame, app: &mut App) {
    // A filter row only exists while it is in use, so the lists keep their
    // height in the common case.
    let filter_height = if app.filtering || !app.filter.is_empty() {
        3
    } else {
        0
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Length(filter_height),
            Constraint::Min(6),
            Constraint::Length(6),
            Constraint::Length(4),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_summary(frame, app, chunks[0]);
    if filter_height > 0 {
        draw_filter(frame, app, chunks[1]);
    }

    // The two things the user works with, side by side: what is waiting to be
    // imported, and what the library already holds.
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(chunks[2]);
    draw_queue(frame, app, panes[0]);
    draw_library(frame, app, panes[1]);

    draw_details(frame, app, chunks[3]);
    draw_status(frame, app, chunks[4]);

    let keys: Vec<(&str, &str)> = if app.filtering {
        vec![("Type", "Filter"), ("Enter", "Keep"), ("Esc", "Clear")]
    } else if app.focus == Focus::Queue {
        vec![
            ("Enter", "Import"),
            ("A", "All"),
            ("W", "Search"),
            ("P", "Project"),
            ("Tab", "Pane"),
            ("/", "Filter"),
            ("R", "Rescan"),
            ("S", "Settings"),
            ("F12", "Diag"),
            ("Q", "Quit"),
        ]
    } else {
        vec![
            ("W", "Search"),
            ("P", "Project"),
            ("Tab", "Pane"),
            ("/", "Filter"),
            ("R", "Rescan"),
            ("S", "Settings"),
            ("F12", "Diag"),
            ("Q", "Quit"),
        ]
    };
    frame.render_widget(
        Paragraph::new(hints(&keys, chunks[5].width)).alignment(Alignment::Center),
        chunks[5],
    );
}

/// Where the library is and what is in it — the two facts worth permanent
/// screen space. The remaining paths live on the Diagnostics page.
fn draw_summary(frame: &mut Frame, app: &App, area: Rect) {
    let width = area.width.saturating_sub(2);
    let totals = app.totals;

    let counts = Line::from(vec![
        count_span(totals.parts, "part"),
        Span::styled("  ·  ", Style::default().fg(MUTED)),
        count_span(totals.symbols, "symbol"),
        Span::styled("  ·  ", Style::default().fg(MUTED)),
        count_span(totals.footprints, "footprint"),
        Span::styled("  ·  ", Style::default().fg(MUTED)),
        count_span(totals.models, "model"),
    ]);

    // Which library these counts belong to is the first thing to check after
    // switching projects, so the name rides in the title.
    let title = match app.config.active_project() {
        Some(project) => format!(" KiCad Library — {} ", project.name),
        None => " KiCad Library ".to_string(),
    };

    // Counts first: on a short terminal the layout squeezes this block, and
    // the second line is the one that gets dropped.
    frame.render_widget(
        Paragraph::new(vec![
            counts,
            Line::from(Span::styled(
                fit(&app.config.library_root.display().to_string(), width),
                Style::default().fg(MUTED),
            )),
        ])
        .block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

fn count_span(value: usize, noun: &str) -> Span<'static> {
    let plural = if value == 1 { "" } else { "s" };
    Span::styled(
        format!("{value} {noun}{plural}"),
        Style::default()
            .fg(if value == 0 { MUTED } else { ACCENT })
            .add_modifier(Modifier::BOLD),
    )
}

fn draw_filter(frame: &mut Frame, app: &App, area: Rect) {
    let caret = if app.filtering { "_" } else { "" };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("  {}{caret}", app.filter),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Filter ")
                .border_style(Style::default().fg(if app.filtering { ACCENT } else { MUTED })),
        ),
        area,
    );
}

fn draw_queue(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Queue;
    let title = if app.filter.is_empty() {
        format!(" ZIP Queue ({}) ", app.zips.len())
    } else {
        format!(" ZIP Queue ({}/{}) ", app.queue_view.len(), app.zips.len())
    };
    let block = pane_block(title, focused);

    if app.queue_view.is_empty() {
        let message = if app.zips.is_empty() {
            vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  No ZIP files found.",
                    Style::default().fg(Color::Yellow),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  Drop Component Search Engine archives into",
                    Style::default().fg(MUTED),
                )),
                Line::from(Span::styled(
                    format!(
                        "  {}",
                        fit(
                            &app.config.download_dir.display().to_string(),
                            area.width.saturating_sub(4)
                        )
                    ),
                    Style::default().fg(MUTED),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  or press [W] to search for a part online.",
                    Style::default().fg(MUTED),
                )),
            ]
        } else {
            vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("  Nothing matches \"{}\".", app.filter),
                    Style::default().fg(MUTED),
                )),
            ]
        };
        frame.render_widget(
            Paragraph::new(message)
                .block(block)
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }

    let items: Vec<ListItem> = app
        .queue_view
        .iter()
        .filter_map(|&i| app.zips.get(i))
        .map(|z| {
            ListItem::new(Line::from(vec![
                Span::raw(z.display.clone()),
                Span::styled(
                    format!("  {}", human_size(z.size)),
                    Style::default().fg(MUTED),
                ),
            ]))
        })
        .collect();

    frame.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(highlight(focused))
            .highlight_symbol(if focused { "> " } else { "  " }),
        area,
        &mut app.list,
    );
}

fn draw_library(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Library;
    let title = if app.filter.is_empty() {
        format!(" Library ({}) ", app.library.len())
    } else {
        format!(
            " Library ({}/{}) ",
            app.library_view.len(),
            app.library.len()
        )
    };
    let block = pane_block(title, focused);

    if app.library_view.is_empty() {
        let message = if app.library.is_empty() {
            vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Nothing imported yet.",
                    Style::default().fg(MUTED),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  Import an archive, or press [W] to search",
                    Style::default().fg(MUTED),
                )),
                Line::from(Span::styled(
                    "  the LCSC catalogue and add a part directly.",
                    Style::default().fg(MUTED),
                )),
            ]
        } else {
            vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("  Nothing matches \"{}\".", app.filter),
                    Style::default().fg(MUTED),
                )),
            ]
        };
        frame.render_widget(
            Paragraph::new(message)
                .block(block)
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }

    // `S F M` marks which of the three file kinds the part actually has, so a
    // part missing a 3D model is visible at a glance.
    let items: Vec<ListItem> = app
        .library_view
        .iter()
        .filter_map(|&i| app.library.get(i))
        .map(|part| {
            // A part found in the folders rather than imported is dimmed, so
            // the pane shows the whole library without claiming credit for it.
            let found = crate::library::is_found(part);
            let name = Style::default();
            let name = if found { name.fg(MUTED) } else { name };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<24}", truncate(&part.name, 24)), name),
                have("S", part.symbol_file.is_some()),
                have("F", !part.footprints.is_empty()),
                have("M", !part.models.is_empty()),
                Span::styled(format!("  {}", part.source), Style::default().fg(MUTED)),
            ]))
        })
        .collect();

    frame.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(highlight(focused))
            .highlight_symbol(if focused { "> " } else { "  " }),
        area,
        &mut app.library_list,
    );
}

fn have(letter: &str, present: bool) -> Span<'static> {
    Span::styled(
        if present {
            format!("{letter} ")
        } else {
            "· ".to_string()
        },
        Style::default().fg(if present { Color::Green } else { MUTED }),
    )
}

/// Whatever the focused pane is pointing at, in full.
fn draw_details(frame: &mut Frame, app: &App, area: Rect) {
    let width = area.width.saturating_sub(2);
    let lines: Vec<Line> = match app.focus {
        Focus::Queue => match app.selected_zip() {
            Some(zip) => vec![
                Line::from(vec![
                    Span::styled(
                        zip.display.clone(),
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("   {}", human_size(zip.size)),
                        Style::default().fg(MUTED),
                    ),
                ]),
                Line::from(Span::styled(
                    fit(&zip.path.display().to_string(), width),
                    Style::default().fg(MUTED),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "[Enter] imports this archive; [A] imports the whole queue.",
                    Style::default().fg(MUTED),
                )),
            ],
            None => vec![Line::from(Span::styled(
                "No archive selected.",
                Style::default().fg(MUTED),
            ))],
        },
        Focus::Library => match app.selected_part() {
            Some(part) => {
                let date = part
                    .imported_at
                    .split('T')
                    .next()
                    .unwrap_or(&part.imported_at);
                // A found part has no provenance to report, only a file date.
                let provenance = if crate::library::is_found(part) {
                    format!("   already in the library   {date}")
                } else {
                    format!("   from {}   on {date}", part.source)
                };
                vec![
                    Line::from(vec![
                        Span::styled(
                            part.name.clone(),
                            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(provenance, Style::default().fg(MUTED)),
                    ]),
                    detail_line("symbol", part.symbol_file.iter().cloned().collect(), width),
                    detail_line("footprint", part.footprints.clone(), width),
                    detail_line("model", part.models.clone(), width),
                ]
            }
            None => vec![Line::from(Span::styled(
                "No part selected.",
                Style::default().fg(MUTED),
            ))],
        },
    };

    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Details ")),
        area,
    );
}

fn detail_line(label: &str, values: Vec<String>, width: u16) -> Line<'static> {
    let text = if values.is_empty() {
        "none".to_string()
    } else {
        values.join(", ")
    };
    let label = format!("{label}: ");
    let room = width.saturating_sub(label.len() as u16);
    Line::from(vec![
        Span::styled(label, Style::default().fg(MUTED)),
        Span::raw(fit(&text, room)),
    ])
}

fn pane_block(title: String, focused: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(if focused { ACCENT } else { MUTED }))
}

/// An unfocused pane keeps its selection tinted rather than barred, so the
/// user does not lose their place but only one caret is ever on screen.
fn highlight(focused: bool) -> Style {
    if focused {
        Style::default()
            .bg(ACCENT)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(ACCENT)
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(" Status ");
    let inner_width = area.width.saturating_sub(2);

    if let Some(import) = &app.import {
        let inner = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(block.inner(area));
        frame.render_widget(block, area);

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                fit(&import.name, inner_width),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ))),
            inner[0],
        );
        frame.render_widget(
            Gauge::default()
                .gauge_style(Style::default().fg(ACCENT))
                .ratio(import.fraction.clamp(0.0, 1.0)),
            inner[1],
        );
        return;
    }

    let watching = if app.config.download_dir.is_dir() {
        app.config.download_dir.display().to_string()
    } else {
        format!("{} (missing)", app.config.download_dir.display())
    };
    let status_style = if app.status.starts_with("Import") && app.status.contains("failed")
        || app.status.starts_with("Scan error")
        || app.status.starts_with("Watch failed")
    {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::Green)
    };

    // Fixed two lines, never wrapped, so a long path cannot push the status
    // message out of the box.
    let text = vec![
        Line::from(vec![
            Span::styled("Watching  ", Style::default().fg(MUTED)),
            Span::raw(fit(&watching, inner_width.saturating_sub(10))),
        ]),
        Line::from(Span::styled(fit(&app.status, inner_width), status_style)),
    ];
    frame.render_widget(Paragraph::new(text).block(block), area);
}

// ----------------------------------------------------------- settings screen

fn draw_settings(frame: &mut Frame, app: &App) {
    let Some(settings) = app.settings.as_ref() else {
        return;
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),
            Constraint::Length(4),
            Constraint::Length(1),
        ])
        .split(frame.area());

    let value_width = chunks[0].width.saturating_sub(6);

    // Each field occupies a label line, a value line and a blank line. Scroll
    // by whole fields so the cursor is always on screen.
    const PER_FIELD: usize = 3;
    let inner = chunks[0].height.saturating_sub(2) as usize;
    let visible = (inner / PER_FIELD).max(1);
    let first = settings.cursor.saturating_sub(visible.saturating_sub(1));
    let last = (first + visible).min(Field::ALL.len());

    let mut lines = Vec::new();
    for (i, field) in Field::ALL.iter().enumerate().take(last).skip(first) {
        let selected = i == settings.cursor;
        let marker = if selected { "> " } else { "  " };
        let label_style = if selected {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(MUTED)
        };
        lines.push(Line::from(vec![
            Span::styled(marker, label_style),
            Span::styled(field.label().to_string(), label_style),
        ]));

        // The row being typed into shows the buffer plus a caret. While
        // editing, keep the tail visible so the cursor position is meaningful.
        let value = match (&settings.editing, selected) {
            (Some(buffer), true) => {
                let shown = if buffer.chars().count() > value_width as usize {
                    let skip = buffer.chars().count() - value_width as usize + 1;
                    buffer.chars().skip(skip).collect::<String>()
                } else {
                    buffer.clone()
                };
                format!("{shown}_")
            }
            _ => fit(&field.display(&settings.draft), value_width),
        };
        let value_style = if selected && settings.editing.is_some() {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else if selected {
            Style::default().fg(Color::White)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(
            format!("    {value}"),
            value_style,
        )));
        lines.push(Line::from(""));
    }

    let mut title = format!(" SETTINGS  ({}/{}) ", settings.cursor + 1, Field::ALL.len());
    if settings.dirty {
        title = format!(
            " SETTINGS  ({}/{})  •unsaved ",
            settings.cursor + 1,
            Field::ALL.len()
        );
    }
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title)),
        chunks[0],
    );

    // What the last edit did beyond its own row, then where the file lives.
    // The config file itself is chosen with --config, not editable here.
    let footer = if settings.note.is_empty() {
        vec![
            Line::from(Span::styled(
                "The library lives under its root; changing the root moves it all. [P] switches projects.",
                Style::default().fg(MUTED),
            )),
            Line::from(Span::raw(format!(
                "  {}",
                fit(
                    &app.config_path.display().to_string(),
                    chunks[1].width.saturating_sub(4)
                )
            ))),
        ]
    } else {
        vec![
            Line::from(Span::styled(
                fit(&settings.note, chunks[1].width.saturating_sub(2)),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Nothing is written until [S]ave.",
                Style::default().fg(MUTED),
            )),
        ]
    };
    frame.render_widget(
        Paragraph::new(footer).block(Block::default().borders(Borders::ALL)),
        chunks[1],
    );

    let keys = if settings.editing.is_some() {
        hints(
            &[("Enter", "Commit"), ("Esc", "Cancel edit")],
            chunks[2].width,
        )
    } else {
        hints(
            &[
                ("E", "Edit"),
                ("Space", "Toggle"),
                ("Tab", "Next"),
                ("Enter", "Browse"),
                ("S", "Save"),
                ("Esc", "Cancel"),
            ],
            chunks[2].width,
        )
    };
    frame.render_widget(Paragraph::new(keys).alignment(Alignment::Center), chunks[2]);
}

// ---------------------------------------------------------- project screen

fn draw_projects(frame: &mut Frame, app: &App) {
    let Some(projects) = app.projects.as_ref() else {
        return;
    };
    let naming = projects.naming.is_some();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if naming { 3 } else { 0 }),
            Constraint::Min(4),
            Constraint::Length(5),
            Constraint::Length(1),
        ])
        .split(frame.area());

    if let Some(name) = projects.naming.as_ref() {
        let title = if name.target.is_some() {
            " Rename Project "
        } else {
            " Name This Location "
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("  {}_", name.buffer),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(Style::default().fg(ACCENT)),
            ),
            chunks[0],
        );
    }

    let list = &app.config.projects;
    let width = chunks[1].width.saturating_sub(6);
    if list.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  No saved projects yet.",
                    Style::default().fg(Color::Yellow),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  [N] saves where you are now, so you can come back to it",
                    Style::default().fg(MUTED),
                )),
                Line::from(Span::styled(
                    "  in one keypress. [B] browses to another library and saves",
                    Style::default().fg(MUTED),
                )),
                Line::from(Span::styled("  that one too.", Style::default().fg(MUTED))),
                Line::from(""),
                Line::from(Span::styled(
                    "  Switching moves the symbol, footprint, 3D, temp and",
                    Style::default().fg(MUTED),
                )),
                Line::from(Span::styled(
                    "  archive folders along with the root.",
                    Style::default().fg(MUTED),
                )),
            ])
            .block(Block::default().borders(Borders::ALL).title(" Projects ")),
            chunks[1],
        );
    } else {
        // Two lines per project: name and marker, then where it lives.
        let height = chunks[1].height.saturating_sub(2) as usize;
        let per_row = 2;
        let visible = (height / per_row).max(1);
        let first = projects.cursor.saturating_sub(visible.saturating_sub(1));

        let mut lines = Vec::new();
        for (i, project) in list.iter().enumerate().skip(first).take(visible) {
            let selected = i == projects.cursor;
            let active = project.library_root == app.config.library_root;
            let style = if selected {
                Style::default()
                    .bg(ACCENT)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            } else if active {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            lines.push(Line::from(vec![
                Span::styled(if selected { "> " } else { "  " }, style),
                Span::styled(if active { "● " } else { "  " }, style),
                Span::styled(project.name.clone(), style),
            ]));
            // A bookmark can outlive the folder it points at; say so rather
            // than letting the switch fail later. The marker gets its own room
            // so it is never the thing that falls off the right edge.
            let exists = project.library_root.is_dir();
            const MISSING: &str = "  (missing)";
            let room = if exists {
                width
            } else {
                width.saturating_sub(MISSING.len() as u16)
            };
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(
                    fit(&project.library_root.display().to_string(), room),
                    Style::default().fg(if exists { MUTED } else { Color::Red }),
                ),
                Span::styled(
                    if exists { "" } else { MISSING },
                    Style::default().fg(Color::Red),
                ),
            ]));
        }

        frame.render_widget(
            Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(format!(
                " Projects ({}/{}) ",
                projects.cursor + 1,
                list.len()
            ))),
            chunks[1],
        );
    }

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                fit(&projects.message, chunks[2].width.saturating_sub(2)),
                Style::default().fg(Color::Green),
            )),
            Line::from(Span::styled("Now:", Style::default().fg(MUTED))),
            Line::from(Span::raw(format!(
                "  {}",
                fit(
                    &app.config.library_root.display().to_string(),
                    chunks[2].width.saturating_sub(4)
                )
            ))),
        ])
        .block(Block::default().borders(Borders::ALL)),
        chunks[2],
    );

    let keys = if naming {
        hints(
            &[
                ("Enter", "Save Name"),
                ("Ctrl-U", "Clear"),
                ("Esc", "Cancel"),
            ],
            chunks[3].width,
        )
    } else {
        hints(
            &[
                ("Enter", "Switch"),
                ("B", "Browse"),
                ("N", "Save Current"),
                ("E", "Rename"),
                ("D", "Forget"),
                ("↑↓", "Move"),
                ("Esc", "Back"),
            ],
            chunks[3].width,
        )
    };
    frame.render_widget(Paragraph::new(keys).alignment(Alignment::Center), chunks[3]);
}

// ------------------------------------------------------------- web search

fn draw_search(frame: &mut Frame, app: &App) {
    let Some(search) = app.search.as_ref() else {
        return;
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(4),
            Constraint::Length(1),
        ])
        .split(frame.area());

    // Query box. The caret only shows while the box has focus.
    let caret = if search.editing { "_" } else { "" };
    let query_style = if search.editing {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("  {}{caret}", search.query),
            query_style,
        )))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Search LCSC / EasyEDA ")
                .border_style(if search.editing {
                    Style::default().fg(ACCENT)
                } else {
                    Style::default().fg(MUTED)
                }),
        ),
        chunks[0],
    );

    // Results.
    let title = format!(" Results ({}) ", search.results.len());
    if search.results.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("  {}", search.message),
                    Style::default().fg(MUTED),
                )),
            ])
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: true }),
            chunks[1],
        );
    } else {
        let width = chunks[1].width.saturating_sub(4) as usize;
        let height = chunks[1].height.saturating_sub(2) as usize;
        let offset = search.cursor.saturating_sub(height.saturating_sub(1));

        let lines: Vec<Line> = search
            .results
            .iter()
            .enumerate()
            .skip(offset)
            .map(|(i, hit)| {
                let stock = if hit.stock > 0 {
                    format!("{} in stock", hit.stock)
                } else {
                    "no stock".to_string()
                };
                let detail = [
                    hit.manufacturer.as_str(),
                    hit.package.as_str(),
                    hit.category.as_str(),
                    &stock,
                ]
                .iter()
                .filter(|s| !s.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join("  ·  ");
                // Already-imported parts are marked so a long result list does
                // not lead to re-adding what the library already has.
                let owned = app.library_has(hit);
                let mark = if owned { "✓ " } else { "  " };
                let text = format!("{mark}{:<10} {:<24} {detail}", hit.id, hit.mpn);
                let text = fit(&text, width as u16);
                if i == search.cursor {
                    Line::from(Span::styled(
                        format!("> {text}"),
                        Style::default()
                            .bg(ACCENT)
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD),
                    ))
                } else if owned {
                    Line::from(Span::styled(
                        format!("  {text}"),
                        Style::default().fg(Color::Green),
                    ))
                } else {
                    Line::from(Span::raw(format!("  {text}")))
                }
            })
            .collect();

        frame.render_widget(
            Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title)),
            chunks[1],
        );
    }

    draw_search_status(frame, app, search, chunks[2]);

    let keys = if search.editing {
        hints(
            &[("Enter", "Search"), ("↓", "Results"), ("Esc", "Back")],
            chunks[3].width,
        )
    } else {
        hints(
            &[
                ("Enter", "Add to Library"),
                ("↑↓", "Move"),
                ("/", "Edit Query"),
                ("Esc", "Back"),
            ],
            chunks[3].width,
        )
    };
    frame.render_widget(Paragraph::new(keys).alignment(Alignment::Center), chunks[3]);
}

fn draw_search_status(frame: &mut Frame, app: &App, search: &crate::app::Search, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(" Status ");

    if let Some(import) = &app.import {
        let inner = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(block.inner(area));
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                fit(&import.name, inner[0].width),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ))),
            inner[0],
        );
        frame.render_widget(
            Gauge::default()
                .gauge_style(Style::default().fg(ACCENT))
                .ratio(import.fraction.clamp(0.0, 1.0)),
            inner[1],
        );
        return;
    }

    let style = if search.message.starts_with("Search failed")
        || search.message.starts_with("Import failed")
    {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::Green)
    };
    let width = area.width.saturating_sub(2);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(fit(&search.message, width), style)),
            Line::from(Span::styled(
                "Symbols, footprints and STEP models go into the configured library.",
                Style::default().fg(MUTED),
            )),
        ])
        .block(block)
        .wrap(Wrap { trim: true }),
        area,
    );
}

// ------------------------------------------------------------------- picker

fn draw_picker(frame: &mut Frame, app: &App) {
    let Some(picker) = app.picker.as_ref() else {
        return;
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(4),
            Constraint::Length(1),
        ])
        .split(frame.area());

    let target_label = picker.target.label();
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!("Choosing: {target_label}"),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::raw(picker.cwd.display().to_string())),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Current Directory "),
        ),
        chunks[0],
    );

    if let Some(error) = &picker.error {
        frame.render_widget(
            Paragraph::new(Span::styled(error.clone(), Style::default().fg(Color::Red)))
                .block(Block::default().borders(Borders::ALL))
                .wrap(Wrap { trim: true }),
            chunks[1],
        );
    } else {
        let items: Vec<Line> = if picker.entries.is_empty() {
            vec![Line::from(Span::styled(
                "  (no subfolders)",
                Style::default().fg(MUTED),
            ))]
        } else {
            picker
                .entries
                .iter()
                .enumerate()
                .map(|(i, path)| {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if i == picker.selected {
                        Line::from(Span::styled(
                            format!("> {name}/"),
                            Style::default()
                                .bg(ACCENT)
                                .fg(Color::Black)
                                .add_modifier(Modifier::BOLD),
                        ))
                    } else {
                        Line::from(Span::raw(format!("  {name}/")))
                    }
                })
                .collect()
        };
        // Keep the highlighted row on screen for long directories.
        let height = chunks[1].height.saturating_sub(2) as usize;
        let offset = picker.selected.saturating_sub(height.saturating_sub(1));
        let visible: Vec<Line> = items.into_iter().skip(offset).collect();

        frame.render_widget(
            Paragraph::new(visible)
                .block(Block::default().borders(Borders::ALL).title(" Folders ")),
            chunks[1],
        );
    }

    frame.render_widget(
        Paragraph::new(hints(
            &[
                ("↑↓", "Move"),
                ("Enter", "Open Folder"),
                ("Backspace", "Parent"),
                ("Space", "Select This Folder"),
                ("Esc", "Cancel"),
            ],
            chunks[2].width,
        ))
        .alignment(Alignment::Center),
        chunks[2],
    );
}

// -------------------------------------------------------------- diagnostics

fn draw_diagnostics(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(1)])
        .split(frame.area());

    let mut lines = Vec::new();

    lines.extend(row("Working Directory", app.cwd.clone()));
    lines.extend(row("Executable", app.exe.clone()));
    lines.extend(row("Config File", app.config_path.display().to_string()));
    lines.extend(row(
        "Log File",
        crate::logging::path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<not initialised>".to_string()),
    ));
    lines.push(Line::from(""));

    for field in Field::DIRS {
        let exists = field.dir(&app.config).map(|p| p.is_dir()).unwrap_or(false);
        lines.push(Line::from(vec![
            Span::styled(field.label().to_string(), Style::default().fg(ACCENT)),
            Span::styled(
                if exists { "  [ok]" } else { "  [MISSING]" },
                Style::default().fg(if exists { Color::Green } else { Color::Red }),
            ),
        ]));
        lines.push(Line::from(Span::raw(format!(
            "  {}",
            field.display(&app.config)
        ))));
    }

    // File-valued settings: present or not yet created, neither is an error.
    for field in [Field::MergedSymbolLib, Field::ManifestPath] {
        let state = match field.path(&app.config) {
            None => ("  [disabled]", Color::DarkGray),
            Some(p) if p.is_file() => ("  [ok]", Color::Green),
            Some(_) => ("  [not created yet]", Color::Yellow),
        };
        lines.push(Line::from(vec![
            Span::styled(field.label().to_string(), Style::default().fg(ACCENT)),
            Span::styled(state.0, Style::default().fg(state.1)),
        ]));
        lines.push(Line::from(Span::raw(format!(
            "  {}",
            field.display(&app.config)
        ))));
    }
    for field in [
        Field::PerPartFootprintDirs,
        Field::BackupBeforeOverwrite,
        Field::DeleteZip,
    ] {
        lines.extend(row(field.label(), field.display(&app.config)));
    }
    lines.push(Line::from(""));

    if app.config.projects.is_empty() {
        lines.extend(row("Saved Projects", "none".to_string()));
    } else {
        lines.push(Line::from(Span::styled(
            "Saved Projects",
            Style::default().fg(ACCENT),
        )));
        for project in &app.config.projects {
            let active = project.library_root == app.config.library_root;
            lines.push(Line::from(Span::raw(format!(
                "  {} {} — {}",
                if active { "●" } else { " " },
                project.name,
                project.library_root.display()
            ))));
        }
    }
    lines.push(Line::from(""));

    lines.extend(row("ZIP Files Found", app.zips.len().to_string()));
    lines.extend(row(
        "Library Contents",
        format!(
            "{} part(s), {} symbol(s), {} footprint(s), {} model(s)",
            app.totals.parts, app.totals.symbols, app.totals.footprints, app.totals.models
        ),
    ));
    lines.extend(row(
        "Last Scan",
        app.last_scan
            .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "never".to_string()),
    ));
    lines.push(Line::from(""));

    if app.fs_errors.is_empty() {
        lines.push(Line::from(Span::styled(
            "No filesystem errors recorded.",
            Style::default().fg(Color::Green),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            format!("Filesystem errors ({}):", app.fs_errors.len()),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
        for error in app.fs_errors.iter().rev().take(10) {
            lines.push(Line::from(Span::styled(
                format!("  {error}"),
                Style::default().fg(Color::Red),
            )));
        }
    }

    // Paths are shown in full here on purpose — this page exists to debug them
    // — so track how tall the wrapped content is and allow scrolling.
    let inner_width = chunks[0].width.saturating_sub(2).max(1);
    let view = chunks[0].height.saturating_sub(2);
    let height: usize = lines
        .iter()
        .map(|l| {
            let w = l.width().max(1);
            w.div_ceil(inner_width as usize)
        })
        .sum();
    app.diag_lines = height.min(u16::MAX as usize) as u16;
    app.diag_view = view;
    let scroll = app.diag_scroll.min(app.diag_lines.saturating_sub(view));

    let title = if app.diag_lines > view {
        format!(" DIAGNOSTICS  (line {}/{}) ", scroll + 1, app.diag_lines)
    } else {
        " DIAGNOSTICS ".to_string()
    };

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(hints(
            &[("↑↓", "Scroll"), ("Esc", "Back"), ("F12", "Back")],
            chunks[1].width,
        ))
        .alignment(Alignment::Center),
        chunks[1],
    );
}

// -------------------------------------------------------- startup validation

fn draw_validate(frame: &mut Frame, app: &App) {
    let Some(validate) = app.validate.as_ref() else {
        return;
    };
    let Some(&field) = validate.missing.get(validate.index) else {
        return;
    };

    // Only the two locations the user chooses are ever asked about; the folders
    // inside the library come with the root, and this says so.
    let inside: Vec<String> = if field == Field::LibraryRoot {
        app.config
            .library_dirs()
            .iter()
            .skip(1)
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect()
    } else {
        Vec::new()
    };

    let remaining = validate.missing.len();
    let mut keys = vec![("Y", "Create"), ("C", "Change Path"), ("Q", "Quit")];
    if remaining > 1 {
        keys.insert(1, ("A", "Create Both"));
    }

    let mut lines = vec![
        Line::from(Span::styled(
            "Directory does not exist:",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::raw(format!("  {}", field.display(&app.config)))),
        Line::from(Span::styled(
            format!("  ({})", field.label()),
            Style::default().fg(MUTED),
        )),
        Line::from(""),
    ];
    if !inside.is_empty() {
        lines.push(Line::from(Span::styled(
            "[Y] creates it and the library folders inside it:",
            Style::default().fg(MUTED),
        )));
        lines.push(Line::from(Span::raw(format!("  {}", inside.join(", ")))));
        lines.push(Line::from(""));
    }
    if remaining > 1 {
        lines.push(Line::from(Span::styled(
            format!("{} more location needs attention.", remaining - 1),
            Style::default().fg(MUTED),
        )));
        lines.push(Line::from(""));
    }
    lines.push(hints(&keys, area_width(frame)));

    // A long library path wraps, so size the box to the wrapped height rather
    // than the line count — the key bar must never be the part that is cut off.
    let inner = area_width(frame).max(1) as usize;
    let height: usize = lines.iter().map(|l| l.width().max(1).div_ceil(inner)).sum();
    let area = centered(frame.area(), 74, height as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Startup Validation ")
                    .border_style(Style::default().fg(Color::Yellow)),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// The width the validation modal will get, needed before it is laid out.
fn area_width(frame: &Frame) -> u16 {
    74u16.min(frame.area().width).saturating_sub(2)
}

/// A diagnostics entry: label on its own line, full value indented beneath, so
/// that wrapping only ever breaks the value.
fn row(label: &str, value: String) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(label.to_string(), Style::default().fg(ACCENT))),
        Line::from(Span::raw(format!("  {value}"))),
    ]
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
