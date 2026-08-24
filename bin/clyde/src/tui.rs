//! The terminal client (Phase 4 deliverable 7).
//!
//! A client of the same admin API as the CLI: no privileged path exists only in
//! the TUI. It shows missions, tasks, pending approvals with their full policy
//! detail, and the mission timeline.
//!
//! Deciding an approval is deliberately not bound to a bare keypress. An
//! approval is the moment the whole design exists for, and a single keystroke in
//! a list view is exactly the reflexive-approval shape the design is trying to
//! avoid; the TUI shows the detail and hands the operator the command.

use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use clyde_api::admin::methods;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::client::{Client, ClientError};
use crate::output;

/// Which pane has focus.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Pane {
    #[default]
    Missions,
    Approvals,
    Timeline,
}

impl Pane {
    fn next(self) -> Self {
        match self {
            Self::Missions => Self::Approvals,
            Self::Approvals => Self::Timeline,
            Self::Timeline => Self::Missions,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Missions => "missions",
            Self::Approvals => "approvals",
            Self::Timeline => "timeline",
        }
    }
}

/// The data the view renders.
#[derive(Debug, Default)]
struct State {
    missions: Vec<serde_json::Value>,
    approvals: Vec<serde_json::Value>,
    timeline: Vec<serde_json::Value>,
    tasks: Vec<serde_json::Value>,
    selected: usize,
    pane: Pane,
    status: String,
}

impl State {
    fn selected_mission(&self) -> Option<&serde_json::Value> {
        self.missions.get(self.selected)
    }

    fn items(&self) -> &[serde_json::Value] {
        match self.pane {
            Pane::Missions => &self.missions,
            Pane::Approvals => &self.approvals,
            Pane::Timeline => &self.timeline,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.items().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        let next = self.selected as isize + delta;
        self.selected = next.rem_euclid(len as isize) as usize;
    }
}

/// Runs the terminal client.
pub async fn run(socket: &Path) -> Result<(), ClientError> {
    let mut client = Client::new(socket);
    let mut state = State::default();
    refresh(&mut client, &mut state).await;

    enable_raw_mode().map_err(|error| ClientError::Protocol(error.to_string()))?;
    let mut stdout = std::io::stdout();
    let _ = crossterm::execute!(stdout, EnterAlternateScreen);
    let backend = CrosstermBackend::new(stdout);
    let mut terminal =
        Terminal::new(backend).map_err(|error| ClientError::Protocol(error.to_string()))?;

    let outcome = event_loop(&mut terminal, &mut client, &mut state).await;

    // The terminal is restored even when the loop failed, because leaving a
    // developer in raw mode with no echo is worse than any error message.
    let _ = disable_raw_mode();
    let _ = crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();
    if let Some(command) = state.status.strip_prefix("run: ") {
        let mut stdout = std::io::stdout();
        let _ = writeln!(stdout, "{command}");
    }
    outcome
}

async fn event_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    client: &mut Client,
    state: &mut State,
) -> Result<(), ClientError> {
    loop {
        terminal
            .draw(|frame| draw(frame, state))
            .map_err(|error| ClientError::Protocol(error.to_string()))?;

        if !event::poll(Duration::from_millis(250))
            .map_err(|error| ClientError::Protocol(error.to_string()))?
        {
            continue;
        }
        let Ok(Event::Key(key)) = event::read() else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Tab => {
                state.pane = state.pane.next();
                state.selected = 0;
            }
            KeyCode::Down | KeyCode::Char('j') => state.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => state.move_selection(-1),
            KeyCode::Char('r') => refresh(client, state).await,
            KeyCode::Enter => {
                // Hands the operator the exact command rather than performing
                // the decision from a list view.
                state.status = command_for(state);
                if state.status.starts_with("run: ") {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
}

/// The command the selected item would need.
fn command_for(state: &State) -> String {
    match state.pane {
        Pane::Approvals => match state.approvals.get(state.selected) {
            Some(approval) => format!(
                "run: clyde approvals approve {}",
                output::scalar(&approval["approval"])
            ),
            None => "nothing selected".to_owned(),
        },
        Pane::Missions => match state.selected_mission() {
            Some(mission) => format!(
                "run: clyde mission review {}",
                output::scalar(&mission["mission"])
            ),
            None => "nothing selected".to_owned(),
        },
        Pane::Timeline => "the timeline is read-only".to_owned(),
    }
}

async fn refresh(client: &mut Client, state: &mut State) {
    state.missions = as_array(
        client
            .call(methods::MISSION_LIST, serde_json::json!({}))
            .await,
    );
    state.approvals = as_array(
        client
            .call(methods::APPROVALS_LIST, serde_json::json!({}))
            .await,
    );
    let mission = state
        .selected_mission()
        .map(|mission| output::scalar(&mission["mission"]));
    state.timeline = as_array(
        client
            .call(
                methods::AUDIT_SHOW,
                serde_json::json!({"mission": mission, "limit": 200}),
            )
            .await,
    );
    state.tasks = match &mission {
        Some(mission) => as_array(
            client
                .call(methods::TASK_LIST, serde_json::json!({"mission": mission}))
                .await,
        ),
        None => Vec::new(),
    };
    state.status = format!(
        "{} mission(s), {} pending approval(s)",
        state.missions.len(),
        state.approvals.len()
    );
}

fn as_array(result: Result<serde_json::Value, ClientError>) -> Vec<serde_json::Value> {
    match result {
        Ok(serde_json::Value::Array(items)) => items,
        _ => Vec::new(),
    }
}

fn draw(frame: &mut ratatui::Frame<'_>, state: &State) {
    // Destructured rather than indexed, so the layout's shape is checked at
    // compile time instead of at runtime.
    let [list_area, detail_area, help_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),
            Constraint::Length(12),
            Constraint::Length(3),
        ])
        .areas(frame.area());

    let items: Vec<ListItem> = state
        .items()
        .iter()
        .map(|item| ListItem::new(summarise(state.pane, item)))
        .collect();
    let mut list_state = ListState::default();
    list_state.select((!items.is_empty()).then_some(state.selected));
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", state.pane.title())),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, list_area, &mut list_state);

    let detail = Paragraph::new(detail_for(state))
        .block(Block::default().borders(Borders::ALL).title(" detail "))
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, detail_area);

    let help = Paragraph::new(Line::from(vec![
        Span::styled("tab", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" pane  "),
        Span::styled("j/k", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" move  "),
        Span::styled("r", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" refresh  "),
        Span::styled("enter", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" command  "),
        Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!(" quit    {}", state.status)),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(help, help_area);
}

/// One line per item.
fn summarise(pane: Pane, item: &serde_json::Value) -> String {
    match pane {
        Pane::Missions => format!(
            "{}  {:<20}  {}",
            output::scalar(&item["mission"]),
            output::scalar(&item["state"]),
            output::scalar(&item["objective"])
        ),
        Pane::Approvals => format!(
            "{}  {:<22}  {}",
            output::scalar(&item["approval"]),
            output::scalar(&item["subject"]),
            output::scalar(&item["summary"])
        ),
        Pane::Timeline => format!(
            "{}{:>6}  {}",
            if item["high_signal"].as_bool().unwrap_or(false) {
                "! "
            } else {
                "  "
            },
            output::scalar(&item["seq"]),
            output::scalar(&item["detail"])
        ),
    }
}

/// The detail pane shows an approval's full policy detail, because that is what
/// the decision needs.
fn detail_for(state: &State) -> String {
    match state.pane {
        Pane::Approvals => state
            .approvals
            .get(state.selected)
            .map(output::approval)
            .unwrap_or_else(|| "no pending approvals".to_owned()),
        Pane::Missions => match state.selected_mission() {
            Some(mission) => {
                let tasks = state
                    .tasks
                    .iter()
                    .map(|task| {
                        format!(
                            "  {} {} {}",
                            output::scalar(&task["task"]),
                            output::scalar(&task["state"]),
                            output::scalar(&task["classification"])
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("{}\n\ntasks:\n{tasks}", output::key_values(mission))
            }
            None => "no missions".to_owned(),
        },
        Pane::Timeline => state
            .timeline
            .get(state.selected)
            .map(output::key_values)
            .unwrap_or_else(|| "no audit events".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]
    use super::*;

    fn state() -> State {
        State {
            missions: vec![serde_json::json!({
                "mission": "m-1", "state": "active", "objective": "tidy"
            })],
            approvals: vec![serde_json::json!({
                "approval": "ap-1",
                "subject": "task_escalation",
                "summary": "fetch dependencies",
                "mission": "m-1",
                "actor": "agent:claude",
                "reason": "missing dependencies",
                "prior_failure": null,
                "alternatives": [],
                "egress_hosts": [],
                "egress_profile": "rust-registry",
                "credentials": "none",
                "inventory_diff": [],
                "task_evidence": [],
                "caveats": [],
                "expires_at": "t",
            })],
            timeline: vec![serde_json::json!({
                "seq": 1, "detail": "mission.proposed", "high_signal": false
            })],
            tasks: Vec::new(),
            selected: 0,
            pane: Pane::Missions,
            status: String::new(),
        }
    }

    #[test]
    fn panes_cycle() {
        assert_eq!(Pane::Missions.next(), Pane::Approvals);
        assert_eq!(Pane::Approvals.next(), Pane::Timeline);
        assert_eq!(Pane::Timeline.next(), Pane::Missions);
    }

    #[test]
    fn selection_wraps_and_survives_an_empty_list() {
        let mut state = state();
        state.move_selection(1);
        assert_eq!(state.selected, 0, "one item wraps to itself");
        state.missions.clear();
        state.move_selection(1);
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn deciding_hands_over_a_command_rather_than_acting_from_a_list_view() {
        let mut state = state();
        state.pane = Pane::Approvals;
        let command = command_for(&state);
        assert_eq!(command, "run: clyde approvals approve ap-1");
        state.pane = Pane::Timeline;
        assert!(command_for(&state).contains("read-only"));
    }

    #[test]
    fn the_detail_pane_shows_an_approvals_full_policy_detail() {
        let mut state = state();
        state.pane = Pane::Approvals;
        let detail = detail_for(&state);
        assert!(detail.contains("rust-registry"));
        assert!(detail.contains("fetch dependencies"));
    }

    #[test]
    fn high_signal_timeline_entries_are_marked() {
        let item = serde_json::json!({"seq": 2, "detail": "drift", "high_signal": true});
        assert!(summarise(Pane::Timeline, &item).starts_with('!'));
    }
}
