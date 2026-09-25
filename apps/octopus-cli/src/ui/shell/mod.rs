use std::io;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::soul::KimiSoul;

pub mod render;

pub struct ShellUI {
    soul: Option<KimiSoul>,
    soul_arc: Option<Arc<tokio::sync::Mutex<Option<KimiSoul>>>>,
    input: String,
    cursor_position: usize,
    messages: Vec<(String, String)>,
    mode: ShellMode,
    exit: bool,
    thinking: bool,
    state: AppState,
    // Cached values for drawing when soul is running
    cached_model_name: String,
    cached_plan_mode: bool,
    // Completion state
    completions: Vec<(String, String)>,
    completion_index: usize,
    completion_scroll_offset: usize,
    show_completions: bool,
    // Ctrl-C tips shown below input area (stack on repeated presses)
    ctrl_c_tips: Vec<String>,
    // Welcome panel inside TUI
    show_welcome: bool,
    // Input area width for cursor calculation
    last_frame_width: u16,
    // Approval flow
    approval_runtime: Option<crate::approval_runtime::ApprovalRuntime>,
    wire_hub_receiver: Option<tokio::sync::broadcast::Receiver<crate::wire::WireEvent>>,
    pending_approval: Option<crate::wire::ApprovalRequestEvent>,
    // Input history
    history: Vec<String>,
    history_index: Option<usize>,
    history_draft: String,
    // External editor
    open_external_editor: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellMode {
    Agent,
    Shell,
}

enum AppState {
    Idle,
    Running(tokio::task::JoinHandle<crate::exception::Result<String>>),
}

impl ShellUI {
    pub fn new(soul: KimiSoul) -> Self {
        let cached_model_name = soul
            .llm
            .as_ref()
            .map(|l| l.model_name.clone())
            .unwrap_or_else(|| "no model".to_string());
        let cached_plan_mode = soul.plan_mode;
        let approval_runtime = Some(soul.approval.runtime().clone());
        let wire_hub_receiver = soul.root_wire_hub.as_ref().map(|h| h.subscribe());
        let history = Self::load_history();
        Self {
            soul: Some(soul),
            soul_arc: None,
            input: String::new(),
            cursor_position: 0,
            messages: Vec::new(),
            mode: ShellMode::Agent,
            exit: false,
            thinking: false,
            state: AppState::Idle,
            cached_model_name,
            cached_plan_mode,
            completions: Vec::new(), // Vec<(name, description)>
            completion_index: 0,
            completion_scroll_offset: 0,
            show_completions: false,
            ctrl_c_tips: Vec::new(),
            show_welcome: true,
            last_frame_width: 80,
            approval_runtime,
            wire_hub_receiver,
            pending_approval: None,
            history,
            history_index: None,
            history_draft: String::new(),
            open_external_editor: false,
        }
    }

    pub async fn run(&mut self, initial_prompt: Option<String>) -> io::Result<bool> {
        crossterm::terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen,)?;

        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        if let Some(prompt) = initial_prompt {
            self.input = prompt;
            self.cursor_position = self.input.len();
            self.submit_input().await;
        }

        let result = self.run_loop(&mut terminal).await;

        crossterm::terminal::disable_raw_mode()?;
        crossterm::execute!(
            terminal.backend_mut(),
            crossterm::terminal::LeaveAlternateScreen,
        )?;
        terminal.show_cursor()?;

        // Shutdown background tasks
        if let Some(ref mut soul) = self.soul {
            soul.shutdown().await;
        } else if let Some(arc) = self.soul_arc.take()
            && let Ok(mut guard) = arc.try_lock()
            && let Some(ref mut soul) = guard.as_mut()
        {
            soul.shutdown().await;
        }

        self.save_history();
        println!("Bye!");

        result
    }

    fn welcome_lines(&self) -> Vec<Line<'static>> {
        let version = crate::constant::get_version();
        let model = self.cached_model_name.clone();

        let gray = Style::default().fg(Color::Gray);
        let yellow = Style::default().fg(Color::Yellow);
        let white = Style::default().fg(Color::White);

        vec![
            Line::from(""),
            Line::from(vec![
                Span::styled("  Model: ", gray),
                Span::styled(model, white),
            ]),
            Line::from(""),
            Line::from(vec![Span::styled(
                "  Tip: Spot a bug or have feedback? Type /feedback right in this session – every report makes Kimi better.",
                gray,
            )]),
            Line::from(""),
            Line::from(vec![Span::styled(
                format!(
                    "  New version available: {}. Run `cargo install --path octopus-cli` to upgrade.",
                    version
                ),
                yellow,
            )]),
            Line::from(""),
        ]
    }

    async fn run_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<bool> {
        let mut reader = crossterm::event::EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_millis(50));

        while !self.exit {
            terminal.draw(|f| self.draw(f))?;

            self.check_task_completion().await;

            tokio::select! {
                _ = tick.tick() => {},
                Some(Ok(event)) = reader.next() => {
                    if let Event::Key(key) = event {
                        self.handle_key_event(key).await;
                    }
                    if self.open_external_editor {
                        self.open_external_editor = false;
                        self.run_external_editor(terminal).await?;
                    }
                }
                Ok(event) = async {
                    if let Some(rx) = self.wire_hub_receiver.as_mut() {
                        rx.recv().await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    match event {
                        crate::wire::WireEvent::ApprovalRequest(req) => {
                            self.pending_approval = Some(req);
                        }
                        crate::wire::WireEvent::HookResolved(resolved) if resolved.action == "block"
                            && !resolved.reason.is_empty() => {
                                self.messages.push((
                                    "system".to_string(),
                                    format!("Hook blocked {}: {}", resolved.event, resolved.reason),
                                ));
                            }
                        // HookTriggered is a progress indicator for GUI clients;
                        // shell mode has no transient status area so we ignore it.
                        _ => {}
                    }
                }
            }
        }

        Ok(true)
    }

    async fn check_task_completion(&mut self) {
        if let AppState::Running(handle) = &mut self.state
            && handle.is_finished()
        {
            match handle.await {
                Ok(result) => {
                    if let Some(arc) = self.soul_arc.take()
                        && let Ok(mut guard) = arc.try_lock()
                    {
                        self.soul = guard.take();
                    }
                    self.state = AppState::Idle;
                    self.thinking = false;
                    match result {
                        Ok(response) => {
                            self.messages.push(("assistant".to_string(), response));
                        }
                        Err(e) => {
                            self.messages
                                .push(("error".to_string(), format!("Error: {}", e)));
                        }
                    }
                }
                Err(e) if e.is_cancelled() => {
                    if let Some(arc) = self.soul_arc.take()
                        && let Ok(mut guard) = arc.try_lock()
                    {
                        self.soul = guard.take();
                    }
                    self.state = AppState::Idle;
                    self.thinking = false;
                    self.messages
                        .push(("system".to_string(), "Interrupted by user".to_string()));
                }
                Err(e) => {
                    if let Some(arc) = self.soul_arc.take()
                        && let Ok(mut guard) = arc.try_lock()
                    {
                        self.soul = guard.take();
                    }
                    self.state = AppState::Idle;
                    self.thinking = false;
                    self.messages
                        .push(("error".to_string(), format!("Task panicked: {}", e)));
                }
            }
        }
    }

    async fn handle_key_event(&mut self, key: KeyEvent) {
        // Handle approval prompt first
        if let Some(ref pending) = self.pending_approval {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if let Some(ref rt) = self.approval_runtime {
                        rt.resolve(
                            &pending.id,
                            crate::approval_runtime::ApprovalResponse::Allow {
                                scope: crate::approval_runtime::ApprovalScope::Once,
                            },
                        );
                    }
                    self.pending_approval = None;
                    return;
                }
                KeyCode::Char('n') | KeyCode::Char('N') => {
                    if let Some(ref rt) = self.approval_runtime {
                        rt.resolve(
                            &pending.id,
                            crate::approval_runtime::ApprovalResponse::Reject {
                                feedback: "User rejected".to_string(),
                            },
                        );
                    }
                    self.pending_approval = None;
                    return;
                }
                KeyCode::Char('a') | KeyCode::Char('A') => {
                    if let Some(ref rt) = self.approval_runtime {
                        rt.resolve(
                            &pending.id,
                            crate::approval_runtime::ApprovalResponse::Allow {
                                scope: crate::approval_runtime::ApprovalScope::Session,
                            },
                        );
                    }
                    self.pending_approval = None;
                    return;
                }
                _ => {}
            }
        }

        // If completions are showing, handle navigation first
        if self.show_completions {
            match key.code {
                KeyCode::Up => {
                    if self.completion_index > 0 {
                        self.completion_index -= 1;
                    }
                    self.adjust_completion_scroll();
                    return;
                }
                KeyCode::Down => {
                    if self.completion_index + 1 < self.completions.len() {
                        self.completion_index += 1;
                    }
                    self.adjust_completion_scroll();
                    return;
                }
                KeyCode::Enter | KeyCode::Tab => {
                    self.accept_completion();
                    return;
                }
                KeyCode::Esc => {
                    self.show_completions = false;
                    return;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Enter => {
                if key.modifiers.contains(KeyModifiers::ALT) {
                    self.insert_char('\n');
                    self.refresh_completions();
                } else {
                    self.submit_input().await;
                }
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_char('\n');
                self.refresh_completions();
            }
            KeyCode::Char('x') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_mode();
            }
            KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                match crate::utils::clipboard::paste_text() {
                    Ok(text) => {
                        for c in text.chars() {
                            self.insert_char(c);
                        }
                        self.refresh_completions();
                    }
                    Err(e) => {
                        self.messages
                            .push(("system".to_string(), format!("Paste failed: {}", e)));
                    }
                }
            }
            KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Find the last assistant message and copy it to clipboard
                let last_assistant = self
                    .messages
                    .iter()
                    .rev()
                    .find(|(role, _)| role == "assistant")
                    .map(|(_, content)| content.clone());
                match last_assistant {
                    Some(text) => match crate::utils::clipboard::copy_text(&text) {
                        Ok(()) => {
                            self.messages.push((
                                "system".to_string(),
                                "Copied last assistant message to clipboard.".to_string(),
                            ));
                        }
                        Err(e) => {
                            self.messages
                                .push(("system".to_string(), format!("Copy failed: {}", e)));
                        }
                    },
                    None => {
                        self.messages.push((
                            "system".to_string(),
                            "No assistant message to copy.".to_string(),
                        ));
                    }
                }
            }
            KeyCode::BackTab => {
                if let Some(ref mut soul) = self.soul {
                    soul.toggle_plan_mode();
                    self.cached_plan_mode = soul.plan_mode;
                }
            }
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_external_editor = true;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                match &self.state {
                    AppState::Idle => {
                        // Ctrl-C never exits; stack tips below input area
                        self.ctrl_c_tips
                            .push("Tip: press Ctrl-D or send 'exit' to quit".to_string());
                    }
                    AppState::Running(handle) => {
                        handle.abort();
                        // check_task_completion will handle the cancellation
                    }
                }
            }
            KeyCode::Char('d')
                if key.modifiers.contains(KeyModifiers::CONTROL) && self.input.is_empty() =>
            {
                self.exit = true;
            }
            KeyCode::Char('\t') => {
                self.handle_tab();
            }
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.insert_char(c);
                self.refresh_completions();
            }
            KeyCode::Backspace => {
                self.delete_char_before_cursor();
                self.refresh_completions();
            }
            KeyCode::Delete => {
                self.delete_char_at_cursor();
                self.refresh_completions();
            }
            KeyCode::Left if self.cursor_position > 0 => {
                self.cursor_position -= 1;
            }
            KeyCode::Right if self.cursor_position < self.input.len() => {
                self.cursor_position += 1;
            }
            KeyCode::Home => {
                self.cursor_position = 0;
            }
            KeyCode::End => {
                self.cursor_position = self.input.len();
            }
            KeyCode::Up => {
                self.history_navigate_up();
            }
            KeyCode::Down => {
                self.history_navigate_down();
            }
            _ => {}
        }
    }

    fn handle_tab(&mut self) {
        if self.input.starts_with('/') && !self.show_completions {
            self.refresh_completions();
            if !self.completions.is_empty() {
                self.show_completions = true;
                self.completion_index = 0;
            }
        } else if self.show_completions && !self.completions.is_empty() {
            self.completion_index = (self.completion_index + 1) % self.completions.len();
        } else if self.completions.len() == 1 {
            self.accept_completion();
        }
    }

    fn refresh_completions(&mut self) {
        if !self.input.starts_with('/') {
            self.show_completions = false;
            self.completions.clear();
            return;
        }

        let prefix = &self.input[1..];
        let commands = self.get_available_commands();
        self.completions = commands
            .into_iter()
            .filter(|(name, _desc)| name.starts_with(prefix))
            .collect();

        if self.completions.is_empty() {
            self.show_completions = false;
            self.completion_scroll_offset = 0;
        } else {
            self.show_completions = true;
            self.completion_index = 0;
            self.completion_scroll_offset = 0;
        }
    }

    fn get_available_commands(&self) -> Vec<(String, String)> {
        let mut commands: Vec<(String, String)> = Vec::new();

        // Hardcoded fallback commands when soul is not available (e.g. during task execution)
        let fallback = vec![
            ("add-dir".to_string(), "Add a directory to the workspace. Usage: /add-dir <path>. Run without args to list added dirs".to_string()),
            ("afk".to_string(), "Toggle afk mode (auto-dismiss AskUserQuestion, auto-approve tool calls)".to_string()),
            ("changelog".to_string(), "Show release notes".to_string()),
            ("clear".to_string(), "Clear the context".to_string()),
            ("compact".to_string(), "Compact the context (optionally with a custom focus, e.g. /compact keep db discussions)".to_string()),
            ("debug".to_string(), "Debug the context".to_string()),
            ("exit".to_string(), "Exit the CLI".to_string()),
            ("feedback".to_string(), "Submit feedback to make Kimi Code CLI better".to_string()),
            ("fork".to_string(), "Fork the current session (copy all history to a new session)".to_string()),
            ("help".to_string(), "Show help information".to_string()),
            ("hooks".to_string(), "List configured hooks".to_string()),
            ("mcp".to_string(), "Show MCP servers and tools".to_string()),
            ("model".to_string(), "Show or switch the current model".to_string()),
            ("new".to_string(), "Start a new session".to_string()),
            ("plan".to_string(), "Toggle plan mode. Usage: /plan [on|off|view|clear]".to_string()),
            ("sessions".to_string(), "List sessions and resume optionally".to_string()),
            ("theme".to_string(), "Switch terminal color theme (dark/light)".to_string()),
            ("title".to_string(), "Set or show the session title".to_string()),
            ("undo".to_string(), "Undo: fork the session at a previous turn and retry".to_string()),
            ("version".to_string(), "Show version information".to_string()),
            ("vis".to_string(), "Open Kimi Agent Tracing Visualizer in browser".to_string()),
            ("web".to_string(), "Open Kimi Code Web UI in browser".to_string()),
            ("yolo".to_string(), "Toggle YOLO mode (auto-approve all actions)".to_string()),
        ];
        commands.extend(fallback);

        // Add commands from soul's slash registry if available
        if let Some(ref soul) = self.soul {
            for (name, desc, aliases) in soul.list_slash_commands() {
                commands.push((name, desc.clone()));
                for alias in aliases {
                    commands.push((alias, desc.clone()));
                }
            }
        }

        commands.sort_by(|a, b| a.0.cmp(&b.0));
        commands.dedup_by(|a, b| a.0 == b.0);
        commands
    }

    fn adjust_completion_scroll(&mut self) {
        const MAX_VISIBLE: usize = 10;
        if self.completion_index < self.completion_scroll_offset {
            self.completion_scroll_offset = self.completion_index;
        } else if self.completion_index >= self.completion_scroll_offset + MAX_VISIBLE {
            self.completion_scroll_offset = self.completion_index.saturating_sub(MAX_VISIBLE - 1);
        }
    }

    fn accept_completion(&mut self) {
        if self.completions.is_empty() {
            return;
        }
        let idx = self.completion_index.min(self.completions.len() - 1);
        let (name, _desc) = &self.completions[idx];
        self.input = format!("/{}", name);
        self.cursor_position = self.input.len();
        self.show_completions = false;
        self.completion_scroll_offset = 0;
    }

    fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor_position, c);
        self.cursor_position += 1;
    }

    fn delete_char_before_cursor(&mut self) {
        if self.cursor_position > 0 {
            self.cursor_position -= 1;
            self.input.remove(self.cursor_position);
        }
    }

    fn delete_char_at_cursor(&mut self) {
        if self.cursor_position < self.input.len() {
            self.input.remove(self.cursor_position);
        }
    }

    fn toggle_mode(&mut self) {
        self.mode = match self.mode {
            ShellMode::Agent => ShellMode::Shell,
            ShellMode::Shell => ShellMode::Agent,
        };
    }

    fn history_path() -> std::path::PathBuf {
        crate::share::get_share_dir().join("shell_history.txt")
    }

    fn load_history() -> Vec<String> {
        let path = Self::history_path();
        if !path.exists() {
            return Vec::new();
        }
        match std::fs::read_to_string(&path) {
            Ok(content) => content
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| l.to_string())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    fn save_history(&self) {
        let path = Self::history_path();
        const MAX_HISTORY: usize = 1000;
        let start = self.history.len().saturating_sub(MAX_HISTORY);
        let content = self.history[start..].join("\n");
        let _ = std::fs::write(&path, content);
    }

    fn push_history(&mut self, input: String) {
        if input.is_empty() {
            return;
        }
        // Avoid duplicate consecutive entries
        if self.history.last().map(|s| s.as_str()) == Some(&input) {
            return;
        }
        self.history.push(input);
    }

    fn history_navigate_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        if self.history_index.is_none() {
            self.history_draft = self.input.clone();
            self.history_index = Some(self.history.len().saturating_sub(1));
        } else {
            let idx = self.history_index.unwrap();
            if idx > 0 {
                self.history_index = Some(idx - 1);
            }
        }
        if let Some(idx) = self.history_index {
            self.input = self.history[idx].clone();
            self.cursor_position = self.input.len();
        }
    }

    fn history_navigate_down(&mut self) {
        match self.history_index {
            None => {}
            Some(idx) => {
                if idx + 1 < self.history.len() {
                    self.history_index = Some(idx + 1);
                    self.input = self.history[idx + 1].clone();
                    self.cursor_position = self.input.len();
                } else {
                    self.history_index = None;
                    self.input = self.history_draft.clone();
                    self.cursor_position = self.input.len();
                }
            }
        }
    }

    fn get_editor_command() -> Option<String> {
        std::env::var("VISUAL")
            .ok()
            .or_else(|| std::env::var("EDITOR").ok())
            .or_else(|| {
                // Fallback: try common editors
                for editor in &["vim", "vi", "nano", "emacs"] {
                    if std::process::Command::new(editor)
                        .arg("--version")
                        .output()
                        .is_ok()
                    {
                        return Some(editor.to_string());
                    }
                }
                None
            })
    }

    async fn run_external_editor(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<()> {
        let editor = match Self::get_editor_command() {
            Some(cmd) => cmd,
            None => {
                self.messages.push((
                    "system".to_string(),
                    "No editor found. Set $VISUAL or $EDITOR.".to_string(),
                ));
                return Ok(());
            }
        };

        // Create temp file with current input
        let temp_path =
            std::env::temp_dir().join(format!("octopus_shell_input_{}.txt", std::process::id()));
        if let Err(e) = std::fs::write(&temp_path, &self.input) {
            self.messages.push((
                "system".to_string(),
                format!("Failed to write temp file: {}", e),
            ));
            return Ok(());
        }

        // Suspend TUI
        crossterm::terminal::disable_raw_mode()?;
        crossterm::execute!(
            terminal.backend_mut(),
            crossterm::terminal::LeaveAlternateScreen,
        )?;
        terminal.show_cursor()?;

        // Launch editor and wait
        let status = tokio::process::Command::new(&editor)
            .arg(&temp_path)
            .status()
            .await;

        // Restore TUI
        crossterm::terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        *terminal = Terminal::new(backend)?;

        match status {
            Ok(s) if s.success() => match std::fs::read_to_string(&temp_path) {
                Ok(content) => {
                    self.input = content.trim_end().to_string();
                    self.cursor_position = self.input.len();
                }
                Err(e) => {
                    self.messages.push((
                        "system".to_string(),
                        format!("Failed to read temp file: {}", e),
                    ));
                }
            },
            Ok(_) => {
                self.messages.push((
                    "system".to_string(),
                    "Editor exited with error. Input unchanged.".to_string(),
                ));
            }
            Err(e) => {
                self.messages.push((
                    "system".to_string(),
                    format!("Failed to launch editor '{}': {}", editor, e),
                ));
            }
        }

        let _ = std::fs::remove_file(&temp_path);
        Ok(())
    }

    async fn submit_input(&mut self) {
        let input = self.input.trim().to_string();
        if input.is_empty() {
            return;
        }

        // Reset history navigation
        self.history_index = None;
        self.history_draft.clear();

        self.show_welcome = false;
        self.show_completions = false;
        self.completions.clear();
        self.ctrl_c_tips.clear();

        // Handle /exit
        if input == "/exit" {
            self.exit = true;
            return;
        }

        // Handle slash commands
        if let Some(ref mut soul) = self.soul
            && let Some(call) = crate::soul::slash::parse_slash_command_call(&input)
            && let Some(cmd) = soul.slash_registry.get(&call.name)
        {
            let func = cmd.func.clone();
            (func)(soul, &call.args).await;
            self.input.clear();
            self.cursor_position = 0;
            return;
        }

        self.push_history(input.clone());

        let mode_str = match self.mode {
            ShellMode::Agent => "agent",
            ShellMode::Shell => "shell",
        };

        self.messages.push((mode_str.to_string(), input.clone()));
        self.input.clear();
        self.cursor_position = 0;
        self.thinking = true;

        // Run the soul in a background task
        if let Some(soul) = self.soul.take() {
            // Update cache before moving
            self.cached_model_name = soul
                .llm
                .as_ref()
                .map(|l| l.model_name.clone())
                .unwrap_or_else(|| "no model".to_string());
            self.cached_plan_mode = soul.plan_mode;

            let soul_arc = Arc::new(tokio::sync::Mutex::new(Some(soul)));
            self.soul_arc = Some(Arc::clone(&soul_arc));

            let handle = tokio::spawn(async move {
                let mut guard = soul_arc.lock().await;
                let mut soul = guard.take().unwrap();
                let result = soul.run(&input).await;
                *guard = Some(soul);
                result
            });

            self.state = AppState::Running(handle);
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        self.last_frame_width = frame.area().width;

        // Build chat content first so we can size the chat area to its content
        let mut text_lines = Vec::new();

        for (role, content) in &self.messages {
            let prefix = match role.as_str() {
                "agent" => "✨ ",
                "shell" => "$ ",
                "assistant" => "🤖 ",
                "error" => "❌ ",
                "system" => "ℹ️  ",
                "btw" => "💡 ",
                _ => "",
            };
            let style = match role.as_str() {
                "agent" => Style::default().fg(Color::Cyan),
                "shell" => Style::default().fg(Color::Green),
                "assistant" => Style::default().fg(Color::White),
                "error" => Style::default().fg(Color::Red),
                "system" => Style::default().fg(Color::Yellow),
                "btw" => Style::default().fg(Color::Magenta),
                _ => Style::default(),
            };

            if role == "assistant" {
                // Rich markdown rendering for assistant messages
                let rendered = render::render_markdown(content);
                for line in rendered {
                    let mut prefixed = line;
                    if let Some(first) = prefixed.spans.first_mut() {
                        first.content = format!("{}{}", prefix, first.content).into();
                    } else {
                        prefixed
                            .spans
                            .insert(0, Span::styled(prefix.to_string(), style));
                    }
                    text_lines.push(prefixed);
                }
            } else {
                for line in content.lines() {
                    text_lines.push(Line::from(vec![
                        Span::styled(prefix, style),
                        Span::styled(line.to_string(), style),
                    ]));
                }
            }
            text_lines.push(Line::from(""));
        }

        if self.thinking {
            text_lines.push(Line::from(vec![
                Span::styled("🤖 ", Style::default().fg(Color::Yellow)),
                Span::styled(
                    "Thinking...",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
        }

        if self.show_welcome && self.messages.is_empty() && !self.thinking {
            text_lines.extend(self.welcome_lines());
        }

        let chat_content_height = text_lines.len() as u16;
        let input_height = self.calculate_input_height();
        let completion_height = if self.show_completions {
            (self.completions.len() as u16).min(10)
        } else {
            0
        };
        let tip_height = self.ctrl_c_tips.len() as u16;
        // Chat gets exactly the space its content needs (capped at half terminal),
        // then a bottom spacer absorbs extra space so input isn't pushed to bottom.
        let chat_height = chat_content_height.max(3).min(frame.area().height / 2);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(chat_height),
                Constraint::Length(input_height),
                Constraint::Length(completion_height),
                Constraint::Length(tip_height),
                Constraint::Min(0),
            ])
            .split(frame.area());

        let mut chunk_idx = 0;

        // Chat area
        let chat_area = chunks[chunk_idx];
        chunk_idx += 1;

        let chat = Paragraph::new(Text::from(text_lines))
            .wrap(Wrap { trim: false })
            .scroll((0, 0));
        frame.render_widget(chat, chat_area);

        // Input area: top border with embedded title, bottom border as separator
        let input_area = chunks[chunk_idx];
        chunk_idx += 1;
        let mode_indicator = match self.mode {
            ShellMode::Agent => "input",
            ShellMode::Shell => "shell",
        };
        let prompt = match self.mode {
            ShellMode::Agent => "",
            ShellMode::Shell => "$ ",
        };

        let title_text = format!("── {} ──", mode_indicator);
        let input_block = Block::default()
            .title_top(Line::from(Span::styled(
                title_text,
                Style::default().fg(Color::DarkGray),
            )))
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray));

        // Render multiline input
        let input_with_prompt = format!("{}{}", prompt, self.input);
        let input_paragraph = Paragraph::new(input_with_prompt.clone()).block(input_block);
        frame.render_widget(input_paragraph, input_area);

        // Set cursor position
        let cursor_pos = self.calculate_cursor_position(input_area, prompt);
        frame.set_cursor_position(cursor_pos);

        // Completion menu (rendered below input)
        let comp_area = chunks[chunk_idx];
        if self.show_completions && !self.completions.is_empty() && comp_area.height > 0 {
            let cmd_col_width = 18usize;
            let comp_lines: Vec<Line> = self
                .completions
                .iter()
                .enumerate()
                .skip(self.completion_scroll_offset)
                .take(10)
                .map(|(i, (name, desc))| {
                    let is_selected = i == self.completion_index;
                    let prefix = if is_selected { "› " } else { "  " };
                    let cmd_text = format!("{}/{}", prefix, name);
                    let cmd_visual_width = cmd_text.width();
                    let padding = " ".repeat(cmd_col_width.saturating_sub(cmd_visual_width));

                    let cmd_style = if is_selected {
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Gray)
                    };

                    let max_desc_width =
                        (comp_area.width as usize).saturating_sub(cmd_col_width + 2);
                    let desc_text = if max_desc_width > 0 && desc.width() > max_desc_width {
                        let mut truncated = String::new();
                        let mut w = 0usize;
                        for ch in desc.chars() {
                            let cw = UnicodeWidthChar::width(ch).unwrap_or(1);
                            if w + cw + 3 > max_desc_width {
                                break;
                            }
                            truncated.push(ch);
                            w += cw;
                        }
                        format!("{}...", truncated)
                    } else {
                        desc.clone()
                    };

                    Line::from(vec![
                        Span::styled(cmd_text, cmd_style),
                        Span::styled(padding, cmd_style),
                        Span::styled(desc_text, Style::default().fg(Color::DarkGray)),
                    ])
                })
                .collect();

            let comp_widget = Paragraph::new(Text::from(comp_lines));
            frame.render_widget(comp_widget, comp_area);
        }
        chunk_idx += 1;

        // Ctrl-C tips (rendered below completion area)
        if tip_height > 0 {
            let tip_area = chunks[chunk_idx];
            let tip_lines: Vec<Line> = self
                .ctrl_c_tips
                .iter()
                .map(|t| Line::from(Span::styled(t.clone(), Style::default().fg(Color::Yellow))))
                .collect();
            let tip_widget = Paragraph::new(Text::from(tip_lines));
            frame.render_widget(tip_widget, tip_area);
        }

        // Approval overlay
        if let Some(ref pending) = self.pending_approval {
            self.draw_approval_overlay(frame, pending);
        }
    }

    fn draw_approval_overlay(
        &self,
        frame: &mut Frame,
        pending: &crate::wire::ApprovalRequestEvent,
    ) {
        let area = frame.area();
        let width = (area.width as f32 * 0.7).clamp(40.0, 80.0) as u16;
        let height = 12u16.min(area.height.saturating_sub(4)).max(6);
        let x = (area.width.saturating_sub(width)) / 2;
        let y = (area.height.saturating_sub(height)) / 2;
        let overlay_area = Rect::new(x, y, width, height);

        // Clear background
        frame.render_widget(ratatui::widgets::Clear, overlay_area);

        let block = Block::default()
            .title_top(Line::from(Span::styled(
                "── Approval Required ──",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow));

        let mut lines = vec![
            Line::from(""),
            Line::from(vec![
                Span::styled("Action: ", Style::default().fg(Color::Cyan)),
                Span::styled(pending.action.clone(), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("Tool:   ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    pending.tool_call_id.clone(),
                    Style::default().fg(Color::Gray),
                ),
            ]),
            Line::from(""),
        ];

        // Truncate description if too long
        let desc = &pending.description;
        let max_desc_len = (width as usize).saturating_sub(4);
        let desc_display = if desc.len() > max_desc_len {
            format!("{}...", &desc[..max_desc_len.saturating_sub(3)])
        } else {
            desc.clone()
        };
        lines.push(Line::from(Span::styled(
            desc_display,
            Style::default().fg(Color::White),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(
                "[Y]",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Approve  ", Style::default().fg(Color::White)),
            Span::styled(
                "[N]",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Reject  ", Style::default().fg(Color::White)),
            Span::styled(
                "[A]",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Approve for session", Style::default().fg(Color::White)),
        ]));

        let paragraph = Paragraph::new(Text::from(lines)).block(block);
        frame.render_widget(paragraph, overlay_area);
    }

    fn calculate_input_height(&self) -> u16 {
        let prompt_len = match self.mode {
            ShellMode::Agent => 0,
            ShellMode::Shell => "$ ".len(),
        };
        let available_width = (self.last_frame_width as usize).saturating_sub(prompt_len);
        if available_width == 0 {
            return 3;
        }

        let text_with_prompt = format!(
            "{}{}",
            match self.mode {
                ShellMode::Agent => "",
                ShellMode::Shell => "$ ",
            },
            self.input
        );

        let mut lines_needed = 1usize;
        let mut current_line_width = 0usize;
        for c in text_with_prompt.chars() {
            if c == '\n' {
                lines_needed += 1;
                current_line_width = 0;
            } else {
                current_line_width += 1;
                if current_line_width > available_width {
                    lines_needed += 1;
                    current_line_width = 1;
                }
            }
        }

        // +2 for top and bottom borders, min 5 (2 borders + 3 content rows)
        (lines_needed as u16 + 2).clamp(5, 12)
    }

    fn calculate_cursor_position(&self, input_area: Rect, prompt: &str) -> (u16, u16) {
        let available_width = input_area.width as usize;

        let mut cursor_row = input_area.y + 1;
        let mut cursor_col = input_area.x;

        let text_before_cursor = format!("{}{}", prompt, &self.input[..self.cursor_position]);

        let mut current_line_width = 0usize;
        for c in text_before_cursor.chars() {
            if c == '\n' {
                cursor_row += 1;
                cursor_col = input_area.x + 1;
                current_line_width = 0;
            } else {
                let w = c.width().unwrap_or(1);
                cursor_col += w as u16;
                current_line_width += w;
                if current_line_width >= available_width {
                    cursor_row += 1;
                    cursor_col = input_area.x + 1 + w as u16;
                    current_line_width = w;
                }
            }
        }

        (cursor_col, cursor_row)
    }
}
