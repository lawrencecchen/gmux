mod config;
mod git;

use std::{
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand, ValueHint};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
};
use serde::Serialize;
use shlex;

use crate::config::{AppConfig, EntryConfig, load_config, save_config};

const MAX_HOTKEYS: usize = 9;
const BRANCH_REFRESH: Duration = Duration::from_millis(500);
const STATUS_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Parser)]
#[command(
    name = "gmux",
    version,
    about = "Manage git directories and launch editors",
    author
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// List registered directories
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Add or update a directory
    Add {
        /// Directory to register
        #[arg(value_hint = ValueHint::DirPath)]
        path: String,
        /// Editor command override
        #[arg(short, long)]
        editor: Option<String>,
    },
    /// Edit an existing directory entry by index or path
    Edit {
        /// Entry index (1-based) or path
        target: String,
        /// New directory path
        #[arg(long, value_hint = ValueHint::DirPath)]
        path: Option<String>,
        /// New editor command
        #[arg(short, long)]
        editor: Option<String>,
    },
    /// Remove an entry by index or path
    Remove {
        /// Entry index (1-based) or path
        target: String,
    },
    /// Launch the editor for an entry
    Open {
        /// Entry index (1-based) or path
        target: String,
        /// Temporary editor override
        #[arg(short, long)]
        editor: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(command) => run_cli(command),
        None => run_tui(),
    }
}

#[derive(Serialize)]
struct ListEntry {
    index: usize,
    path: String,
    branch: String,
    editor: Option<String>,
}

fn run_cli(command: Command) -> Result<()> {
    match command {
        Command::List { json } => {
            let config = load_config()?;
            let entries: Vec<ListEntry> = config
                .entries
                .iter()
                .enumerate()
                .map(|(idx, entry)| ListEntry {
                    index: idx + 1,
                    path: display_path(&entry.path),
                    branch: branch_state_for(entry).text(),
                    editor: entry.editor.clone(),
                })
                .collect();

            if json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else if entries.is_empty() {
                println!("No directories registered.");
            } else {
                for item in &entries {
                    if let Some(editor) = &item.editor {
                        println!(
                            "{:>2}. {:<40} {:<15} {}",
                            item.index, item.path, item.branch, editor
                        );
                    } else {
                        println!("{:>2}. {:<40} {:<15}", item.index, item.path, item.branch);
                    }
                }
            }
            Ok(())
        }
        Command::Add { path, editor } => add_entry_cli(path, editor),
        Command::Edit {
            target,
            path,
            editor,
        } => edit_entry_cli(target, path, editor),
        Command::Remove { target } => remove_entry_cli(target),
        Command::Open { target, editor } => open_entry_cli(target, editor),
    }
}

fn run_tui() -> Result<()> {
    let mut app = App::new()?;

    enable_terminal()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    app.refresh_branches();
    let mut last_tick = Instant::now();

    let res = loop {
        app.maybe_clear_status();
        terminal.draw(|f| ui(f, &app))?;

        let timeout = BRANCH_REFRESH
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_millis(0));

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                app.handle_key(key);
                if app.should_quit {
                    break Ok(());
                }
            }
        }

        if last_tick.elapsed() >= BRANCH_REFRESH {
            app.refresh_branches();
            last_tick = Instant::now();
        }
    };

    disable_terminal()?;
    res
}

fn enable_terminal() -> Result<()> {
    terminal::enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, cursor::Show)?;
    Ok(())
}

fn disable_terminal() -> Result<()> {
    execute!(io::stdout(), LeaveAlternateScreen, cursor::Show)?;
    terminal::disable_raw_mode()?;
    Ok(())
}

#[derive(Clone, Debug)]
struct Entry {
    config: EntryConfig,
    branch: BranchState,
}

impl Entry {
    fn from_config(config: EntryConfig) -> Self {
        Self {
            config,
            branch: BranchState::Unknown,
        }
    }
}

#[derive(Clone, Debug)]
struct GitBranchInfo {
    name: String,
    additions: u32,
    deletions: u32,
}

impl GitBranchInfo {
    fn summary(&self) -> String {
        let mut changes = Vec::new();
        if self.additions > 0 {
            changes.push(format!("+{}", self.additions));
        }
        if self.deletions > 0 {
            changes.push(format!("-{}", self.deletions));
        }

        if changes.is_empty() {
            self.name.clone()
        } else {
            format!("{} ({})", self.name, changes.join(" "))
        }
    }

    fn spans(&self) -> Vec<Span<'_>> {
        let mut spans = Vec::new();
        spans.push(Span::styled(
            self.name.clone(),
            Style::default().fg(Color::Rgb(120, 170, 255)),
        ));

        if self.additions > 0 || self.deletions > 0 {
            spans.push(Span::raw(" "));
            spans.push(Span::raw("("));
            let mut need_space = false;
            if self.additions > 0 {
                spans.push(Span::styled(
                    format!("+{}", self.additions),
                    Style::default().fg(Color::Green),
                ));
                need_space = true;
            }
            if self.deletions > 0 {
                if need_space {
                    spans.push(Span::raw(" "));
                }
                spans.push(Span::styled(
                    format!("-{}", self.deletions),
                    Style::default().fg(Color::Red),
                ));
            }
            spans.push(Span::raw(")"));
        }

        spans
    }
}

#[derive(Clone, Debug)]
enum BranchState {
    Unknown,
    Ready(GitBranchInfo),
    Missing,
    NotGit,
    Error(String),
}

impl BranchState {
    fn label(&self) -> Vec<Span<'_>> {
        match self {
            BranchState::Unknown => vec![Span::styled("…", Style::default().fg(Color::DarkGray))],
            BranchState::Ready(info) => info.spans(),
            BranchState::Missing => vec![Span::styled("missing", Style::default().fg(Color::Red))],
            BranchState::NotGit => vec![Span::styled(
                "not a repo",
                Style::default().fg(Color::Yellow),
            )],
            BranchState::Error(err) => {
                vec![Span::styled(err.clone(), Style::default().fg(Color::Red))]
            }
        }
    }

    fn text(&self) -> String {
        match self {
            BranchState::Unknown => "…".to_string(),
            BranchState::Ready(info) => info.summary(),
            BranchState::Missing => "missing".to_string(),
            BranchState::NotGit => "not a repo".to_string(),
            BranchState::Error(err) => err.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    Normal,
    Input { flow: FlowKind, step: FlowStep },
    ConfirmDelete { index: usize },
}

#[derive(Clone, Copy, Debug)]
enum FlowKind {
    Add,
    Edit,
}

#[derive(Clone, Copy, Debug)]
enum FlowStep {
    Directory,
    Editor,
}

struct StatusMessage {
    text: String,
    kind: StatusKind,
    created_at: Instant,
}

#[derive(Copy, Clone, Debug)]
enum StatusKind {
    Info,
    Error,
}

struct App {
    config: AppConfig,
    entries: Vec<Entry>,
    selected: usize,
    mode: Mode,
    input_buffer: String,
    input_cursor: usize,
    kill_buffer: String,
    pending_path: Option<PathBuf>,
    editing_index: Option<usize>,
    status: Option<StatusMessage>,
    should_quit: bool,
}

impl App {
    fn new() -> Result<Self> {
        let config = load_config().unwrap_or_default();
        let entries = config
            .entries
            .iter()
            .cloned()
            .map(Entry::from_config)
            .collect();

        Ok(Self {
            config,
            entries,
            selected: 0,
            mode: Mode::Normal,
            input_buffer: String::new(),
            input_cursor: 0,
            kill_buffer: String::new(),
            pending_path: None,
            editing_index: None,
            status: None,
            should_quit: false,
        })
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return;
        }

        match self.mode {
            Mode::Normal => self.handle_normal_key(key),
            Mode::Input { flow, step } => self.handle_input_key(flow, step, key),
            Mode::ConfirmDelete { index } => self.handle_confirm_delete(index, key),
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('n') => {
                    self.move_selection_down();
                    return;
                }
                KeyCode::Char('p') => {
                    self.move_selection_up();
                    return;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('r') => self.refresh_branches(),
            KeyCode::Char('d') => self.request_remove(),
            KeyCode::Char('a') => self.start_add_flow(),
            KeyCode::Char('e') => self.start_edit_flow(),
            KeyCode::Char('j') => self.move_selection_down(),
            KeyCode::Char('k') => self.move_selection_up(),
            KeyCode::Char(c @ '1'..='9') => {
                let idx = (c as u8 - b'1') as usize;
                if idx < self.entries.len() {
                    self.selected = idx;
                    self.launch_index(idx);
                }
            }
            KeyCode::Enter => self.launch_index(self.selected),
            KeyCode::Up => {
                self.move_selection_up();
            }
            KeyCode::Down => {
                self.move_selection_down();
            }
            _ => {}
        }
    }

    fn handle_input_key(&mut self, flow: FlowKind, step: FlowStep, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if self.handle_ctrl_input(flow, step, key.code) {
                return;
            }
        }

        if key.modifiers.contains(KeyModifiers::SUPER) || key.modifiers.contains(KeyModifiers::META)
        {
            if self.handle_super_input(key.code) {
                return;
            }
        }

        if key.modifiers.contains(KeyModifiers::ALT) {
            if self.handle_alt_input(key.code) {
                return;
            }
        }

        match key.code {
            KeyCode::Esc => self.cancel_flow(),
            KeyCode::Enter => self.submit_flow_step(flow, step),
            KeyCode::Backspace => self.delete_prev_char(),
            KeyCode::Delete => self.delete_char(),
            KeyCode::Left => self.move_cursor_left(),
            KeyCode::Right => self.move_cursor_right(),
            KeyCode::Home => self.cursor_to_start(),
            KeyCode::End => self.cursor_to_end(),
            KeyCode::Char(c) => {
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                    self.insert_char(c);
                }
            }
            _ => {}
        }
    }

    fn handle_confirm_delete(&mut self, index: usize, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.clear_status();
            }
            KeyCode::Enter => {
                match self.remove_entry(index) {
                    Ok(path) => {
                        let path_str = display_path(&path);
                        self.set_status(StatusKind::Info, format!("Removed {path_str}"));
                    }
                    Err(err) => self.set_status(StatusKind::Error, err.to_string()),
                }
                self.mode = Mode::Normal;
            }
            _ => {}
        }
    }

    fn start_add_flow(&mut self) {
        self.mode = Mode::Input {
            flow: FlowKind::Add,
            step: FlowStep::Directory,
        };
        self.input_buffer.clear();
        self.input_cursor = 0;
        self.kill_buffer.clear();
        self.pending_path = None;
        self.editing_index = None;
        self.set_status(StatusKind::Info, "Enter directory path".into());
    }

    fn start_edit_flow(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let idx = self.selected.min(self.entries.len() - 1);
        let entry = self.entries[idx].config.clone();
        self.mode = Mode::Input {
            flow: FlowKind::Edit,
            step: FlowStep::Directory,
        };
        self.input_buffer = entry.path.to_string_lossy().to_string();
        self.input_cursor = self.input_buffer.chars().count();
        self.kill_buffer.clear();
        self.pending_path = Some(entry.path.clone());
        self.editing_index = Some(idx);
        self.set_status(
            StatusKind::Info,
            "Edit directory path and press enter".into(),
        );
    }

    fn request_remove(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let idx = self.selected.min(self.entries.len() - 1);
        let path_str = display_path(&self.entries[idx].config.path);
        self.mode = Mode::ConfirmDelete { index: idx };
        self.set_status(
            StatusKind::Info,
            format!("Press Enter to remove {path_str} or Esc to cancel"),
        );
    }

    fn cancel_flow(&mut self) {
        self.mode = Mode::Normal;
        self.input_buffer.clear();
        self.input_cursor = 0;
        self.pending_path = None;
        self.editing_index = None;
        self.kill_buffer.clear();
    }

    fn complete_directory_step(&mut self, flow: FlowKind) -> Result<()> {
        let raw_path = self.input_buffer.trim();
        if raw_path.is_empty() {
            return Err(anyhow!("directory path cannot be empty"));
        }

        let path = expand_path(raw_path);
        let path_display = display_path(&path);
        if !path.exists() {
            return Err(anyhow!("{path_display} does not exist"));
        }
        if !path.is_dir() {
            return Err(anyhow!("{path_display} is not a directory"));
        }

        self.pending_path = Some(path.clone());
        self.mode = Mode::Input {
            flow,
            step: FlowStep::Editor,
        };
        let prefill = match flow {
            FlowKind::Add => self.editor_prefill(None),
            FlowKind::Edit => {
                let entry = self
                    .editing_index
                    .and_then(|idx| self.config.entries.get(idx));
                self.editor_prefill(entry)
            }
        };
        self.input_buffer = prefill;
        self.input_cursor = self.input_buffer.chars().count();
        let message = match flow {
            FlowKind::Add => "Set editor command (enter to accept current value)".into(),
            FlowKind::Edit => "Edit editor command (enter to accept current value)".into(),
        };
        self.set_status(StatusKind::Info, message);
        Ok(())
    }

    fn complete_editor_step(&mut self, flow: FlowKind) -> Result<()> {
        let path = self
            .pending_path
            .clone()
            .ok_or_else(|| anyhow!("no directory captured"))?;

        let editor_string = self.input_buffer.trim().to_string();
        let editor = if editor_string.is_empty() {
            None
        } else {
            Some(editor_string.clone())
        };

        if let Some(cmd) = editor.clone() {
            self.config.default_editor = Some(cmd);
        }

        match flow {
            FlowKind::Add => {
                self.save_entry(path.clone(), editor.clone())?;
                let path_str = display_path(&path);
                self.set_status(StatusKind::Info, format!("Registered {path_str}"));
            }
            FlowKind::Edit => {
                let idx = self
                    .editing_index
                    .ok_or_else(|| anyhow!("no entry selected to edit"))?;
                self.update_entry(idx, path.clone(), editor.clone())?;
                let path_str = display_path(&path);
                self.set_status(StatusKind::Info, format!("Updated {path_str}"));
            }
        }

        self.mode = Mode::Normal;
        self.input_buffer.clear();
        self.input_cursor = 0;
        self.pending_path = None;
        self.editing_index = None;
        Ok(())
    }

    fn submit_flow_step(&mut self, flow: FlowKind, step: FlowStep) {
        let result = match step {
            FlowStep::Directory => self.complete_directory_step(flow),
            FlowStep::Editor => self.complete_editor_step(flow),
        };
        if let Err(err) = result {
            self.set_status(StatusKind::Error, err.to_string());
        }
    }

    fn handle_ctrl_input(&mut self, flow: FlowKind, step: FlowStep, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('a') | KeyCode::Home => {
                self.cursor_to_start();
                true
            }
            KeyCode::Char('e') | KeyCode::End => {
                self.cursor_to_end();
                true
            }
            KeyCode::Char('b') => {
                self.move_cursor_left();
                true
            }
            KeyCode::Char('f') => {
                self.move_cursor_right();
                true
            }
            KeyCode::Char('d') => {
                self.delete_char();
                true
            }
            KeyCode::Char('h') => {
                self.delete_prev_char();
                true
            }
            KeyCode::Char('k') => {
                self.kill_to_end();
                true
            }
            KeyCode::Char('u') => {
                self.kill_to_start();
                true
            }
            KeyCode::Char('w') | KeyCode::Backspace => {
                self.kill_prev_word();
                true
            }
            KeyCode::Char('y') => {
                self.yank_kill_buffer();
                true
            }
            KeyCode::Char('g') => {
                self.cancel_flow();
                true
            }
            KeyCode::Char('j') | KeyCode::Char('m') | KeyCode::Enter => {
                self.submit_flow_step(flow, step);
                true
            }
            KeyCode::Left => {
                self.move_word_left();
                true
            }
            KeyCode::Right => {
                self.move_word_right();
                true
            }
            KeyCode::Delete => {
                self.kill_word_forward();
                true
            }
            _ => false,
        }
    }

    fn handle_alt_input(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('b') | KeyCode::Left => {
                self.move_word_left();
                true
            }
            KeyCode::Char('f') | KeyCode::Right => {
                self.move_word_right();
                true
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                self.kill_word_forward();
                true
            }
            KeyCode::Backspace => {
                self.kill_prev_word();
                true
            }
            _ => false,
        }
    }

    fn handle_super_input(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Backspace => {
                self.kill_to_start();
                true
            }
            _ => false,
        }
    }

    fn editor_prefill(&self, entry: Option<&EntryConfig>) -> String {
        if let Some(entry) = entry {
            if let Some(cmd) = &entry.editor {
                return cmd.clone();
            }
        }
        if let Some(cmd) = &self.config.default_editor {
            return cmd.clone();
        }
        entry_editor_fallback().unwrap_or_default()
    }

    fn save_entry(&mut self, path: PathBuf, editor: Option<String>) -> Result<()> {
        let entry = EntryConfig {
            path: path.clone(),
            editor: editor.clone(),
        };

        if let Some(existing) = self
            .config
            .entries
            .iter_mut()
            .find(|e| normalize(&e.path) == normalize(&path))
        {
            *existing = entry.clone();
        } else {
            self.config.entries.push(entry.clone());
        }

        save_config(&self.config)?;
        self.sync_entries();
        self.refresh_branches();
        if !self.entries.is_empty() {
            if let Some(idx) = self
                .entries
                .iter()
                .position(|e| normalize(&e.config.path) == normalize(&path))
            {
                self.selected = idx;
            }
        }

        Ok(())
    }

    fn update_entry(&mut self, idx: usize, path: PathBuf, editor: Option<String>) -> Result<()> {
        if idx >= self.config.entries.len() {
            return Err(anyhow!("invalid entry index"));
        }

        if let Some(cmd) = editor.clone() {
            self.config.default_editor = Some(cmd.clone());
        }

        self.config.entries[idx] = EntryConfig {
            path: path.clone(),
            editor: editor.clone(),
        };

        save_config(&self.config)?;
        self.sync_entries();
        self.refresh_branches();
        if !self.entries.is_empty() {
            if let Some(pos) = self
                .entries
                .iter()
                .position(|e| normalize(&e.config.path) == normalize(&path))
            {
                self.selected = pos;
            } else if idx < self.entries.len() {
                self.selected = idx;
            } else {
                self.selected = self.entries.len() - 1;
            }
        } else {
            self.selected = 0;
        }

        Ok(())
    }

    fn remove_entry(&mut self, idx: usize) -> Result<PathBuf> {
        if idx >= self.config.entries.len() {
            return Err(anyhow!("invalid entry index"));
        }

        let removed = self.config.entries.remove(idx);
        save_config(&self.config)?;
        self.sync_entries();
        self.refresh_branches();
        if self.entries.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.entries.len() {
            self.selected = self.entries.len() - 1;
        }

        Ok(removed.path)
    }

    fn sync_entries(&mut self) {
        self.entries = self
            .config
            .entries
            .iter()
            .cloned()
            .map(Entry::from_config)
            .collect();
    }

    fn refresh_branches(&mut self) {
        for entry in &mut self.entries {
            entry.branch = branch_state_for(&entry.config);
        }
    }

    fn move_selection_up(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        if self.selected == 0 {
            self.selected = self.entries.len() - 1;
        } else {
            self.selected -= 1;
        }
    }

    fn move_selection_down(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.entries.len();
    }

    fn buffer_len(&self) -> usize {
        self.input_buffer.chars().count()
    }

    fn cursor_to_start(&mut self) {
        self.input_cursor = 0;
    }

    fn cursor_to_end(&mut self) {
        self.input_cursor = self.buffer_len();
    }

    fn move_cursor_left(&mut self) {
        if self.input_cursor > 0 {
            self.input_cursor -= 1;
        }
    }

    fn move_cursor_right(&mut self) {
        if self.input_cursor < self.buffer_len() {
            self.input_cursor += 1;
        }
    }

    fn move_word_left(&mut self) {
        let new_index = self.word_left_index();
        self.input_cursor = new_index;
    }

    fn move_word_right(&mut self) {
        let new_index = self.word_right_index();
        self.input_cursor = new_index;
    }

    fn insert_char(&mut self, ch: char) {
        let byte_idx = byte_index_at(&self.input_buffer, self.input_cursor);
        self.input_buffer.insert(byte_idx, ch);
        self.input_cursor += 1;
    }

    fn insert_str_at_cursor(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let byte_idx = byte_index_at(&self.input_buffer, self.input_cursor);
        self.input_buffer.insert_str(byte_idx, text);
        self.input_cursor += text.chars().count();
    }

    fn delete_prev_char(&mut self) {
        if self.input_cursor == 0 {
            return;
        }
        let start = byte_index_at(&self.input_buffer, self.input_cursor - 1);
        let end = byte_index_at(&self.input_buffer, self.input_cursor);
        self.input_buffer.drain(start..end);
        self.input_cursor -= 1;
    }

    fn delete_char(&mut self) {
        if self.input_cursor >= self.buffer_len() {
            return;
        }
        let start = byte_index_at(&self.input_buffer, self.input_cursor);
        let end = byte_index_at(&self.input_buffer, self.input_cursor + 1);
        self.input_buffer.drain(start..end);
    }

    fn kill_prev_word(&mut self) {
        let new_cursor = self.word_left_index();
        let removed = self.remove_range(new_cursor, self.input_cursor);
        if !removed.is_empty() {
            self.kill_buffer = removed;
            self.input_cursor = new_cursor;
        }
    }

    fn kill_word_forward(&mut self) {
        let new_index = self.word_right_index();
        let removed = self.remove_range(self.input_cursor, new_index);
        if !removed.is_empty() {
            self.kill_buffer = removed;
        }
    }

    fn kill_to_start(&mut self) {
        let removed = self.remove_range(0, self.input_cursor);
        if !removed.is_empty() {
            self.kill_buffer = removed;
            self.input_cursor = 0;
        }
    }

    fn kill_to_end(&mut self) {
        let removed = self.remove_range(self.input_cursor, self.buffer_len());
        if !removed.is_empty() {
            self.kill_buffer = removed;
        }
    }

    fn yank_kill_buffer(&mut self) {
        if self.kill_buffer.is_empty() {
            return;
        }
        let text = self.kill_buffer.clone();
        self.insert_str_at_cursor(&text);
    }

    fn remove_range(&mut self, start: usize, end: usize) -> String {
        if start >= end {
            return String::new();
        }
        let byte_start = byte_index_at(&self.input_buffer, start);
        let byte_end = byte_index_at(&self.input_buffer, end);
        self.input_buffer.drain(byte_start..byte_end).collect()
    }

    fn word_left_index(&self) -> usize {
        let chars: Vec<char> = self.input_buffer.chars().collect();
        let mut idx = self.input_cursor.min(chars.len());
        while idx > 0 && chars[idx - 1].is_whitespace() {
            idx -= 1;
        }
        while idx > 0 && !chars[idx - 1].is_whitespace() {
            idx -= 1;
        }
        idx
    }

    fn word_right_index(&self) -> usize {
        let chars: Vec<char> = self.input_buffer.chars().collect();
        let mut idx = self.input_cursor.min(chars.len());
        while idx < chars.len() && chars[idx].is_whitespace() {
            idx += 1;
        }
        while idx < chars.len() && !chars[idx].is_whitespace() {
            idx += 1;
        }
        idx
    }

    fn launch_index(&mut self, idx: usize) {
        if idx >= self.entries.len() {
            return;
        }
        if let Err(err) = launch_editor(&self.entries[idx].config) {
            self.set_status(StatusKind::Error, err.to_string());
        } else {
            let path_str = display_path(&self.entries[idx].config.path);
            self.set_status(StatusKind::Info, format!("Opened {path_str}"));
        }
    }

    fn set_status(&mut self, kind: StatusKind, text: String) {
        self.status = Some(StatusMessage {
            text,
            kind,
            created_at: Instant::now(),
        });
    }

    fn clear_status(&mut self) {
        self.status = None;
    }

    fn maybe_clear_status(&mut self) {
        if !matches!(self.mode, Mode::Normal) {
            return;
        }
        if let Some(status) = &self.status {
            if status.created_at.elapsed() >= STATUS_TIMEOUT {
                self.status = None;
            }
        }
    }
}

fn branch_state_for(entry: &EntryConfig) -> BranchState {
    if !entry.path.exists() {
        BranchState::Missing
    } else if !entry.path.is_dir() {
        BranchState::Error("not a dir".into())
    } else if !git::is_git_repo(&entry.path) {
        BranchState::NotGit
    } else {
        match git::current_branch(&entry.path) {
            Ok(branch) => {
                let diff = git::diff_stat(&entry.path).unwrap_or_default();
                let info = GitBranchInfo {
                    name: branch,
                    additions: diff.additions,
                    deletions: diff.deletions,
                };
                BranchState::Ready(info)
            }
            Err(err) => BranchState::Error(err.to_string()),
        }
    }
}

fn add_entry_cli(path: String, editor: Option<String>) -> Result<()> {
    let expanded = expand_path(path.trim());
    let display = display_path(&expanded);
    if !expanded.exists() {
        return Err(anyhow!("{display} does not exist"));
    }
    if !expanded.is_dir() {
        return Err(anyhow!("{display} is not a directory"));
    }

    let editor = normalize_editor_arg(editor);
    let mut config = load_config()?;

    if let Some(cmd) = editor.clone() {
        config.default_editor = Some(cmd.clone());
    }

    let normalized_path = normalize(&expanded);
    if let Some(existing) = config
        .entries
        .iter_mut()
        .find(|entry| normalize(&entry.path) == normalized_path)
    {
        existing.path = expanded.clone();
        existing.editor = editor.clone();
        println!("Updated {display}");
    } else {
        config.entries.push(EntryConfig {
            path: expanded.clone(),
            editor: editor.clone(),
        });
        println!("Added {display}");
    }

    save_config(&config)?;
    Ok(())
}

fn edit_entry_cli(target: String, new_path: Option<String>, editor: Option<String>) -> Result<()> {
    let mut config = load_config()?;
    if config.entries.is_empty() {
        return Err(anyhow!("no entries registered"));
    }

    let idx = resolve_target(&config.entries, &target)
        .ok_or_else(|| anyhow!("entry not found: {target}"))?;

    if new_path.is_none() && editor.is_none() {
        return Err(anyhow!("nothing to update"));
    }

    let mut entry = config.entries[idx].clone();

    if let Some(path_str) = new_path {
        let expanded = expand_path(path_str.trim());
        let display = display_path(&expanded);
        if !expanded.exists() {
            return Err(anyhow!("{display} does not exist"));
        }
        if !expanded.is_dir() {
            return Err(anyhow!("{display} is not a directory"));
        }
        entry.path = expanded;
    }

    if let Some(editor_arg) = editor {
        let normalized = normalize_editor_arg(Some(editor_arg));
        if let Some(cmd) = normalized.clone() {
            config.default_editor = Some(cmd.clone());
        }
        entry.editor = normalized;
    }

    let display = display_path(&entry.path);
    config.entries[idx] = entry;
    save_config(&config)?;
    println!("Updated {display}");
    Ok(())
}

fn remove_entry_cli(target: String) -> Result<()> {
    let mut config = load_config()?;
    if config.entries.is_empty() {
        return Err(anyhow!("no entries registered"));
    }

    let idx = resolve_target(&config.entries, &target)
        .ok_or_else(|| anyhow!("entry not found: {target}"))?;

    let removed = config.entries.remove(idx);
    let display = display_path(&removed.path);
    save_config(&config)?;
    println!("Removed {display}");
    Ok(())
}

fn open_entry_cli(target: String, editor_override: Option<String>) -> Result<()> {
    let config = load_config()?;
    if config.entries.is_empty() {
        return Err(anyhow!("no entries registered"));
    }

    let idx = resolve_target(&config.entries, &target)
        .ok_or_else(|| anyhow!("entry not found: {target}"))?;

    let mut entry = config.entries[idx].clone();
    if let Some(editor_arg) = editor_override {
        entry.editor = normalize_editor_arg(Some(editor_arg));
    }

    let display = display_path(&entry.path);
    launch_editor(&entry)?;
    println!("Opening {display}");
    Ok(())
}

fn resolve_target(entries: &[EntryConfig], target: &str) -> Option<usize> {
    if let Ok(idx) = target.parse::<usize>() {
        if idx >= 1 && idx <= entries.len() {
            return Some(idx - 1);
        }
    }

    let expanded = expand_path(target.trim());
    let normalized = normalize(&expanded);
    entries
        .iter()
        .position(|entry| normalize(&entry.path) == normalized)
}

fn normalize_editor_arg(editor: Option<String>) -> Option<String> {
    editor
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        canonical
    } else {
        path.to_path_buf()
    }
}

fn expand_path(input: &str) -> PathBuf {
    if let Some(stripped) = input.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    }
    PathBuf::from(input)
}

fn launch_editor(entry: &EntryConfig) -> Result<()> {
    let command_string = entry
        .editor
        .clone()
        .or_else(|| entry_editor_fallback())
        .context("no editor set. provide one in the entry or set QUICKSWITCH_EDITOR/EDITOR")?;

    let mut parts = shlex::split(&command_string)
        .with_context(|| format!("failed to parse editor command: {command_string}"))?;

    if parts.is_empty() {
        return Err(anyhow!("editor command is empty"));
    }

    let program = parts.remove(0);
    let mut command = std::process::Command::new(&program);
    command.args(parts);
    command.arg(&entry.path);

    command.spawn().with_context(|| {
        let path_str = display_path(&entry.path);
        format!("failed to launch editor `{}` for {path_str}", program)
    })?;
    Ok(())
}

fn entry_editor_fallback() -> Option<String> {
    std::env::var("QUICKSWITCH_EDITOR")
        .ok()
        .or_else(|| std::env::var("EDITOR").ok())
        .or_else(|| std::env::var("VISUAL").ok())
}

fn byte_index_at(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(idx, _)| idx)
        .unwrap_or_else(|| s.len())
}

fn display_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(stripped) = path.strip_prefix(&home) {
            if stripped.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", stripped.display());
        }

        if let Ok(canonical) = path.canonicalize() {
            if let Ok(stripped) = canonical.strip_prefix(&home) {
                if stripped.as_os_str().is_empty() {
                    return "~".to_string();
                }
                return format!("~/{}", stripped.display());
            }
        }
    }

    path.display().to_string()
}

fn ui(frame: &mut Frame, app: &App) {
    frame.render_widget(Clear, frame.size());

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(5),
        ])
        .split(frame.size());

    let base_style = Style::default();

    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            "gmux",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  — numbers open • j/k or ctrl-n/p move • a add • e edit • d delete (enter) • r refresh",
            Style::default().fg(Color::White),
        ),
    ]))
    .style(base_style);
    frame.render_widget(header, layout[0]);

    let list_block = Block::default()
        .title(Span::styled(
            "Registered directories",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .style(base_style);

    let list_items: Vec<ListItem> = if app.entries.is_empty() {
        vec![ListItem::new(Line::from(vec![Span::styled(
            "No directories registered yet (press 'a' to add)",
            base_style,
        )]))]
    } else {
        app.entries
            .iter()
            .enumerate()
            .map(|(idx, entry)| {
                let hotkey = if idx < MAX_HOTKEYS {
                    format!("{}.", idx + 1)
                } else {
                    "·".into()
                };
                let branch_spans = entry.branch.label();
                let is_selected = idx == app.selected;
                let hotkey_style = if is_selected {
                    Style::default().fg(Color::Rgb(120, 170, 255))
                } else {
                    Style::default().fg(Color::White)
                };

                let mut spans = vec![Span::styled(hotkey, hotkey_style)];
                spans.push(Span::styled(" ", Style::default().fg(Color::White)));
                spans.push(Span::styled(
                    display_path(&entry.config.path),
                    Style::default().fg(Color::White),
                ));
                spans.push(Span::styled("  ", Style::default().fg(Color::White)));
                spans.extend(branch_spans);
                if let Some(editor) = &entry.config.editor {
                    spans.push(Span::styled("  ", Style::default().fg(Color::White)));
                    spans.push(Span::styled(
                        editor.clone(),
                        Style::default().fg(Color::Rgb(150, 150, 150)),
                    ));
                }
                if is_selected && !app.entries.is_empty() {
                    spans.push(Span::styled("  ", Style::default().fg(Color::White)));
                    spans.push(Span::styled(
                        "*",
                        Style::default().fg(Color::Rgb(120, 170, 255)),
                    ));
                }
                ListItem::new(Line::from(spans)).style(base_style)
            })
            .collect()
    };

    let list = List::new(list_items)
        .block(list_block)
        .highlight_style(Style::default());

    let mut list_state = ratatui::widgets::ListState::default();
    if !app.entries.is_empty() {
        list_state.select(Some(app.selected.min(app.entries.len() - 1)));
    }
    frame.render_stateful_widget(list, layout[1], &mut list_state);

    draw_bottom_panel(frame, layout[2], app, base_style);
}

fn draw_bottom_panel(
    frame: &mut Frame,
    area: ratatui::prelude::Rect,
    app: &App,
    base_style: Style,
) {
    match app.mode {
        Mode::Normal => {
            let block = Block::default()
                .title(Span::styled(
                    "Status",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .style(base_style);

            let mut lines = Vec::new();
            if let Some(status) = &app.status {
                let prefix = match status.kind {
                    StatusKind::Info => Span::styled("✔ ", base_style.fg(Color::LightGreen)),
                    StatusKind::Error => Span::styled("✖ ", base_style.fg(Color::Red)),
                };
                lines.push(Line::from(vec![
                    prefix,
                    Span::styled(&status.text, base_style),
                ]));
            } else {
                lines.push(Line::from(Span::styled(
                    "Press number to open • j/k or ctrl-n/p move • a add • e edit • d delete (enter to confirm) • q quit",
                    base_style,
                )));
            }

            let paragraph = Paragraph::new(lines).block(block).style(base_style);
            frame.render_widget(paragraph, area);
        }
        Mode::Input { flow, step } => {
            let (title, hint) = match (flow, step) {
                (FlowKind::Add, FlowStep::Directory) => {
                    ("Add Directory", "Enter to confirm • Esc/Ctrl+G to cancel")
                }
                (FlowKind::Add, FlowStep::Editor) => (
                    "Editor Command",
                    "Enter to accept • Ctrl+A/E/B/F etc. • Esc/Ctrl+G cancels",
                ),
                (FlowKind::Edit, FlowStep::Directory) => (
                    "Edit Directory",
                    "Update path • Enter to confirm • Esc/Ctrl+G cancels",
                ),
                (FlowKind::Edit, FlowStep::Editor) => (
                    "Edit Editor Command",
                    "Enter to accept • Ctrl+A/E/B/F etc. • Esc/Ctrl+G cancels",
                ),
            };

            let block = Block::default()
                .title(Span::styled(
                    title,
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .style(base_style);

            let input_area = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Min(0),
                ])
                .split(area);

            frame.render_widget(block, area);

            let hint_line = Paragraph::new(Line::from(Span::styled(
                hint,
                base_style.fg(Color::Rgb(150, 150, 150)),
            )))
            .style(base_style);
            frame.render_widget(
                hint_line,
                ratatui::prelude::Rect {
                    x: input_area[0].x + 1,
                    y: input_area[0].y + 1,
                    width: input_area[0].width.saturating_sub(2),
                    height: 1,
                },
            );

            let input_line =
                Paragraph::new(Span::styled(&app.input_buffer, base_style)).style(base_style);
            frame.render_widget(
                input_line,
                ratatui::prelude::Rect {
                    x: input_area[1].x + 1,
                    y: input_area[1].y + 1,
                    width: input_area[1].width.saturating_sub(2),
                    height: 1,
                },
            );

            let mut cursor_x = input_area[1].x + 1 + app.input_cursor as u16;
            let cursor_y = input_area[1].y + 1;
            let max_x = input_area[1].x + input_area[1].width.saturating_sub(2);
            if cursor_x > max_x {
                cursor_x = max_x;
            }
            frame.set_cursor(cursor_x, cursor_y);
        }
        Mode::ConfirmDelete { index } => {
            let block = Block::default()
                .title(Span::styled(
                    "Confirm Removal",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .style(base_style);

            frame.render_widget(block, area);

            let path_text = app
                .entries
                .get(index)
                .map(|entry| display_path(&entry.config.path))
                .unwrap_or_else(|| "<unknown>".to_string());

            let lines = vec![
                Line::from(Span::styled(
                    format!("Remove {path_text}?"),
                    Style::default().fg(Color::White),
                )),
                Line::from(Span::styled(
                    "Press Enter to confirm or Esc to cancel",
                    base_style.fg(Color::Rgb(150, 150, 150)),
                )),
            ];

            let content = Paragraph::new(lines).style(base_style);
            frame.render_widget(
                content,
                ratatui::prelude::Rect {
                    x: area.x + 1,
                    y: area.y + 1,
                    width: area.width.saturating_sub(2),
                    height: area.height.saturating_sub(2),
                },
            );
        }
    }
}
