//! Soul-level slash commands — 1:1 Rust rewrite of `kimi_cli/soul/slash.py`.
//!
//! In the Python original, soul-level slash commands are async (or sync) callables
//! registered in a `SlashCommandRegistry[SoulSlashCmdFunc]`. They operate directly
//! on `KimiSoul` without needing UI access. This module mirrors that design
//! exactly, translating Python's dynamic typing into Rust's static type system.
//!
//! Python original:
//!   - `type SoulSlashCmdFunc = Callable[[KimiSoul, str], None | Awaitable[None]]`
//!   - `SlashCommandRegistry[SoulSlashCmdFunc]` with decorator-based registration
//!
//! Rust equivalent:
//!   - `SoulSlashCmdFunc` as an `Arc`-wrapped async trait object (see below)
//!   - `SlashCommandRegistry` with imperative registration (no proc macros yet)

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::soul::KimiSoul;
use crate::wire::{BtwBegin, BtwEnd, StatusUpdate, TextPart};

/// The Rust equivalent of Python's `SoulSlashCmdFunc`.
///
/// Python: `Callable[[KimiSoul, str], None | Awaitable[None]]`
///
/// Because Rust has no garbage collector and requires explicit lifetimes,
/// this type is more verbose. The HRTB (`for<'a>`) lets the closure accept
/// references with any lifetime — needed because each invocation temporarily
/// borrows the soul and the argument string.
///
/// - `Arc<...>` replaces Python's implicit reference counting (the function
///   lives in the registry and may be cloned).
/// - `Pin<Box<dyn Future>>` replaces Python's `Awaitable[None]`.
/// - `+ Send + Sync` ensures thread-safety, which Python got "for free" via
///   the GIL.
pub type SoulSlashCmdFunc = Arc<
    dyn for<'a> Fn(&'a mut KimiSoul, &'a str) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>
        + Send
        + Sync,
>;

/// Mirrors Python's `SlashCommand[F]` dataclass.
///
/// Python stores the raw function object; Rust stores an `Arc`-wrapped trait
/// object (`SoulSlashCmdFunc`) because function types are sized and must be
/// erased to live in a uniform collection.
pub struct SlashCommand {
    pub name: String,
    pub func: SoulSlashCmdFunc,
    pub description: String,
    pub aliases: Vec<String>,
}

/// Mirrors Python's `SlashCommandRegistry[F]`.
///
/// Python keeps two internal dicts (`_commands` and `_command_aliases`).
/// Rust keeps one `HashMap<String, Arc<SlashCommand>>` where both canonical
/// names and aliases point to the same `Arc<SlashCommand>`, achieving the
/// same deduplication behaviour.
pub struct SlashCommandRegistry {
    commands: HashMap<String, Arc<SlashCommand>>,
}

impl SlashCommandRegistry {
    /// Equivalent to Python's `SlashCommandRegistry.__init__`.
    pub fn new() -> Self {
        Self {
            commands: HashMap::new(),
        }
    }

    /// Equivalent to Python's `@registry.command` decorator applied to a
    /// function.  Instead of decorating, we imperatively build a `SlashCommand`
    /// and insert it (and its aliases) into the map.
    pub fn register(&mut self, command: SlashCommand) {
        let arc_cmd = Arc::new(command);
        self.commands.insert(arc_cmd.name.clone(), arc_cmd.clone());
        for alias in &arc_cmd.aliases {
            self.commands.insert(alias.clone(), arc_cmd.clone());
        }
    }

    /// Equivalent to Python's `find_command(name)`.
    pub fn get(&self, name: &str) -> Option<Arc<SlashCommand>> {
        self.commands.get(name).cloned()
    }

    /// Equivalent to Python's `list_commands()` — returns unique primary
    /// commands, deduplicating aliases that map to the same underlying command.
    pub fn list_commands(&self) -> Vec<Arc<SlashCommand>> {
        let mut seen = HashMap::new();
        for cmd in self.commands.values() {
            seen.entry(&cmd.name).or_insert_with(|| cmd.clone());
        }
        seen.into_values().collect()
    }
}

impl Default for SlashCommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Default command registrations
// =============================================================================
// Each block below mirrors a Python async function decorated with
// `@registry.command(...)` in `kimi_cli/soul/slash.py`.
// Where Python uses `wire_send(TextPart(...))`, the Rust code uses
// `crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart { text: ... })`.
// =============================================================================

pub fn build_default_slash_commands() -> SlashCommandRegistry {
    let mut registry = SlashCommandRegistry::new();

    // -------------------------------------------------------------------------
    // /clear  (aliases: /reset)
    // Python: `@registry.command(aliases=["reset"]) async def clear(...)`
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "clear".to_string(),
        func: Arc::new(|soul: &mut KimiSoul, _args: &str| {
            Box::pin(async move {
                if let Err(e) = soul.context.lock().await.clear().await {
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                        text: format!("Failed to clear context: {e}"),
                    }));
                    return;
                }
                let _ = soul
                    .context
                    .lock()
                    .await
                    .write_system_prompt(&soul.agent.system_prompt)
                    .await;
                crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                    text: "The context has been cleared.".to_string(),
                }));
                let snap = soul.status_snapshot().await;
                crate::wire::wire_send(crate::wire::WireEvent::StatusUpdate(StatusUpdate {
                    context_usage: Some(snap.context_usage),
                    context_tokens: Some(snap.context_tokens),
                    max_context_tokens: Some(snap.max_context_tokens),
                    ..Default::default()
                }));
            })
        }),
        description: "Clear the context".to_string(),
        aliases: vec!["reset".to_string()],
    });

    // -------------------------------------------------------------------------
    // /yolo
    // Python: `@registry.command async def yolo(...)`
    // Toggles the explicit auto-approve flag.
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "yolo".to_string(),
        func: Arc::new(|soul: &mut KimiSoul, _args: &str| {
            Box::pin(async move {
                if soul.approval.is_yolo() {
                    soul.approval.toggle_yolo();
                    if soul.approval.state().mode.is_afk() {
                        crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                            text: "Yolo disabled, but afk is still on — tool calls remain auto-approved. Use /afk to turn off afk.".to_string(),
                        }));
                    } else {
                        crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                            text: "You only die once! Actions will require approval.".to_string(),
                        }));
                    }
                } else {
                    soul.approval.toggle_yolo();
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                        text: "You only live once! All actions will be auto-approved.".to_string(),
                    }));
                }
                soul.sync_approval_state();
            })
        }),
        description: "Toggle YOLO mode (auto-approve all actions)".to_string(),
        aliases: Vec::new(),
    });

    // -------------------------------------------------------------------------
    // /afk
    // Python: `@registry.command async def afk(...)`
    // Toggles afk mode (auto-dismiss AskUserQuestion, auto-approve tool calls).
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "afk".to_string(),
        func: Arc::new(|soul: &mut KimiSoul, _args: &str| {
            Box::pin(async move {
                if soul.approval.state().mode.is_afk() {
                    soul.approval.toggle_afk();
                    soul.notify_afk_changed(false).await;
                    let msg = if soul.approval.is_yolo() {
                        "afk mode disabled. You are back at the terminal. Yolo is still on."
                    } else {
                        "afk mode disabled. You are back at the terminal."
                    };
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart { text: msg.to_string() }));
                } else {
                    soul.approval.toggle_afk();
                    soul.notify_afk_changed(true).await;
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                        text: "afk mode enabled. AskUserQuestion will be auto-dismissed and tool calls auto-approved.".to_string(),
                    }));
                }
                soul.sync_approval_state();
            })
        }),
        description: "Toggle afk mode (auto-dismiss AskUserQuestion, auto-approve tool calls)"
            .to_string(),
        aliases: Vec::new(),
    });

    // -------------------------------------------------------------------------
    // /plan  [on|off|view|clear]
    // Python: `@registry.command async def plan(...)`
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "plan".to_string(),
        func: Arc::new(|soul: &mut KimiSoul, args: &str| {
            Box::pin(async move {
                let subcmd = args.trim().to_lowercase();
                match subcmd.as_str() {
                    "on" => {
                        if !soul.plan_mode {
                            soul.toggle_plan_mode();
                        }
                        crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                            text: format!("Plan mode ON. Plan file: {:?}", soul.get_plan_file_path()),
                        }));
                        crate::wire::wire_send(crate::wire::WireEvent::StatusUpdate(StatusUpdate {
                            plan_mode: Some(soul.plan_mode),
                            ..Default::default()
                        }));
                    }
                    "off" => {
                        if soul.plan_mode {
                            soul.toggle_plan_mode();
                        }
                        crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                            text: "Plan mode OFF. All tools are now available.".to_string(),
                        }));
                        crate::wire::wire_send(crate::wire::WireEvent::StatusUpdate(StatusUpdate {
                            plan_mode: Some(soul.plan_mode),
                            ..Default::default()
                        }));
                    }
                    "view" => {
                        let content = soul.read_current_plan().unwrap_or_default();
                        if content.is_empty() {
                            crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                                text: "No plan file found for this session.".to_string(),
                            }));
                        } else {
                            crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart { text: content }));
                        }
                    }
                    "clear" => {
                        soul.clear_current_plan();
                        crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                            text: "Plan cleared.".to_string(),
                        }));
                    }
                    _ => {
                        let new_state = soul.toggle_plan_mode();
                        if new_state {
                            crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                                text: format!(
                                    "Plan mode ON. Write your plan to: {:?}\nUse ExitPlanMode when done, or /plan off to exit manually.",
                                    soul.get_plan_file_path()
                                ),
                            }));
                        } else {
                            crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                                text: "Plan mode OFF. All tools are now available.".to_string(),
                            }));
                        }
                        crate::wire::wire_send(crate::wire::WireEvent::StatusUpdate(StatusUpdate {
                            plan_mode: Some(soul.plan_mode),
                            ..Default::default()
                        }));
                    }
                }
            })
        }),
        description: "Toggle plan mode. Usage: /plan [on|off|view|clear]".to_string(),
        aliases: Vec::new(),
    });

    // -------------------------------------------------------------------------
    // /compact [focus]
    // Python: `@registry.command async def compact(...)`
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "compact".to_string(),
        func: Arc::new(|soul: &mut KimiSoul, args: &str| {
            Box::pin(async move {
                if soul.context.lock().await.n_checkpoints() == 0 {
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                        text: "The context is empty.".to_string(),
                    }));
                    return;
                }
                if let Err(e) = soul.compact_context(args.trim()).await {
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                        text: format!("Compaction failed: {e}"),
                    }));
                    return;
                }
                crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                    text: "The context has been compacted.".to_string(),
                }));
                let snap = soul.status_snapshot().await;
                crate::wire::wire_send(crate::wire::WireEvent::StatusUpdate(StatusUpdate {
                    context_usage: Some(snap.context_usage),
                    context_tokens: Some(snap.context_tokens),
                    max_context_tokens: Some(snap.max_context_tokens),
                    ..Default::default()
                }));
            })
        }),
        description: "Compact the context (optionally with a custom focus, e.g. /compact keep db discussions)".to_string(),
        aliases: Vec::new(),
    });

    // -------------------------------------------------------------------------
    // /help  (aliases: /h, /?)
    // Python soul-level commands do not include /help; it lives in the Shell
    // layer (`kimi_cli/ui/shell/slash.py`). We include a minimal soul-level
    // version here so that headless consumers still have basic discoverability.
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "help".to_string(),
        func: Arc::new(|soul: &mut KimiSoul, _args: &str| {
            Box::pin(async move {
                let mut lines = vec!["Available slash commands:".to_string(), String::new()];
                for cmd in soul.slash_registry.list_commands() {
                    lines.push(format!("  /{} - {}", cmd.name, cmd.description));
                    for alias in &cmd.aliases {
                        lines.push(format!("  /{} - alias for /{}", alias, cmd.name));
                    }
                }
                crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                    text: lines.join("\n"),
                }));
            })
        }),
        description: "Show help information".to_string(),
        aliases: vec!["h".to_string(), "?".to_string()],
    });

    // -------------------------------------------------------------------------
    // /changelog  (aliases: /release-notes)
    // Python: shell-level command; kept here for parity.
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "changelog".to_string(),
        func: Arc::new(|_soul: &mut KimiSoul, _args: &str| {
            Box::pin(async move {
                let changelog_paths = [
                    std::path::PathBuf::from("CHANGELOG.md"),
                    std::path::PathBuf::from(
                        "/home/zw/code/rust_programming/octopus/tmp/kimi-cli/CHANGELOG.md",
                    ),
                ];
                let mut found = None;
                for path in &changelog_paths {
                    if path.exists()
                        && let Ok(content) = tokio::fs::read_to_string(path).await
                    {
                        found = Some(content);
                        break;
                    }
                }
                let text = match found {
                    Some(content) => {
                        let lines: Vec<&str> = content.lines().take(80).collect();
                        format!("Release notes:\n\n{}", lines.join("\n"))
                    }
                    None => "No CHANGELOG.md found.".to_string(),
                };
                crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart { text }));
            })
        }),
        description: "Show release notes".to_string(),
        aliases: vec!["release-notes".to_string()],
    });

    // -------------------------------------------------------------------------
    // /debug
    // Python soul-level command that dumps runtime diagnostics.
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "debug".to_string(),
        func: Arc::new(|soul: &mut KimiSoul, _args: &str| {
            Box::pin(async move {
                let snap = soul.status_snapshot().await;
                let lines = vec![
                    "Debug context:".to_string(),
                    String::new(),
                    format!("  Session ID:     {}", soul.session.id),
                    format!(
                        "  Model:          {}",
                        soul.llm
                            .as_ref()
                            .map(|l| l.model_name.clone())
                            .unwrap_or_else(|| "none".to_string())
                    ),
                    format!("  Plan mode:      {}", soul.plan_mode),
                    format!("  YOLO:           {}", soul.approval.is_yolo()),
                    format!("  AFK:            {}", soul.approval.state().mode.is_afk()),
                    format!(
                        "  Context tokens: {} / {} ({:.1}%)",
                        snap.context_tokens,
                        snap.max_context_tokens,
                        snap.context_usage * 100.0
                    ),
                    format!(
                        "  Checkpoints:    {}",
                        soul.context.lock().await.n_checkpoints()
                    ),
                ];
                crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                    text: lines.join("\n"),
                }));
            })
        }),
        description: "Debug the context".to_string(),
        aliases: Vec::new(),
    });

    // -------------------------------------------------------------------------
    // /add-dir <path>
    // Python: `@registry.command(name="add-dir") async def add_dir(...)`
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "add-dir".to_string(),
        func: Arc::new(|soul: &mut KimiSoul, args: &str| {
            Box::pin(async move {
                let path = args.trim();
                if path.is_empty() {
                    let dirs = &soul.config.workspace_dirs;
                    if dirs.is_empty() {
                        crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                            text: "No additional directories in the workspace.\nUsage: /add-dir <path>".to_string(),
                        }));
                    } else {
                        let mut lines = vec!["Added directories:".to_string()];
                        for d in dirs {
                            lines.push(format!("  - {}", d.display()));
                        }
                        crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                            text: lines.join("\n"),
                        }));
                    }
                    return;
                }
                let p = std::path::PathBuf::from(path);
                if !p.exists() {
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                        text: format!("Path does not exist: {}", path),
                    }));
                    return;
                }
                if !p.is_dir() {
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                        text: format!("Path is not a directory: {}", path),
                    }));
                    return;
                }
                let canonical = p.canonicalize().unwrap_or(p);
                if !soul.config.workspace_dirs.contains(&canonical) {
                    soul.config.workspace_dirs.push(canonical.clone());
                }
                crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                    text: format!("Added directory to workspace: {}", canonical.display()),
                }));
            })
        }),
        description: "Add a directory to the workspace. Usage: /add-dir <path>. Run without args to list added dirs".to_string(),
        aliases: Vec::new(),
    });

    // -------------------------------------------------------------------------
    // /exit  (aliases: /quit)
    // Python shell-level command. Minimal soul-level placeholder.
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "exit".to_string(),
        func: Arc::new(|_soul: &mut KimiSoul, _args: &str| {
            Box::pin(async move {
                crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                    text: "Use Ctrl-D or type 'exit' to quit.".to_string(),
                }));
            })
        }),
        description: "Exit the CLI".to_string(),
        aliases: vec!["quit".to_string()],
    });

    // -------------------------------------------------------------------------
    // /version
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "version".to_string(),
        func: Arc::new(|_soul: &mut KimiSoul, _args: &str| {
            Box::pin(async move {
                crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                    text: format!("kimi, version {}", crate::constant::get_version()),
                }));
            })
        }),
        description: "Show version information".to_string(),
        aliases: Vec::new(),
    });

    // -------------------------------------------------------------------------
    // /model [show | list | <alias>]
    // Python: `@registry.command @shell_mode_registry.command async def model(...)`
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "model".to_string(),
        func: Arc::new(|soul: &mut KimiSoul, args: &str| {
            Box::pin(async move {
                let arg = args.trim();

                // --- show current model ---
                if arg.is_empty() || arg == "show" {
                    let model = soul
                        .llm
                        .as_ref()
                        .map(|l| {
                            let alias = crate::llm::model_display_name(
                                Some(&l.model_name),
                                l.model_config.as_ref(),
                            );
                            if alias.is_empty() {
                                l.model_name.clone()
                            } else {
                                alias
                            }
                        })
                        .unwrap_or_else(|| "no model".to_string());
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                        text: format!("Current model: {}", model),
                    }));
                    return;
                }

                // --- list available models ---
                if arg == "list" {
                    if soul.config.models.is_empty() {
                        crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                            text: "No models configured.".to_string(),
                        }));
                        return;
                    }
                    let mut lines = vec!["Available models:".to_string()];
                    for name in soul.config.models.keys() {
                        let model_cfg = &soul.config.models[name];
                        let display = model_cfg.display_name.as_ref().unwrap_or(&model_cfg.model);
                        let current_marker = soul
                            .llm
                            .as_ref()
                            .and_then(|l| l.model_config.as_ref())
                            .map(|c| c == model_cfg)
                            .unwrap_or(false);
                        let marker = if current_marker { " (current)" } else { "" };
                        lines.push(format!("  - {}{}", display, marker));
                    }
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                        text: lines.join("\n"),
                    }));
                    return;
                }

                // --- switch model ---
                let alias = arg;
                match crate::llm::clone_llm_with_model_alias(
                    soul.llm.as_ref(),
                    &soul.config,
                    Some(alias),
                ) {
                    Ok(Some(new_llm)) => {
                        soul.llm = Some(new_llm);
                        // Persist to config if loaded from default location.
                        if soul.config.is_from_default_location {
                            let mut cfg = soul.config.clone();
                            cfg.default_model = alias.to_string();
                            let _ = crate::config::save_config(&cfg, None);
                        }
                        let display = crate::llm::model_display_name(
                            Some(alias),
                            soul.config.models.get(alias),
                        );
                        crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                            text: format!("Switched to model: {}", display),
                        }));
                    }
                    Ok(None) => {
                        crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                            text: format!("Model '{}' not found in configuration.", alias),
                        }));
                    }
                    Err(e) => {
                        crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                            text: format!("Failed to switch model '{}': {}", alias, e),
                        }));
                    }
                }
            })
        }),
        description: "Show or switch the current model. Usage: /model [show | list | <alias>]"
            .to_string(),
        aliases: Vec::new(),
    });

    // -------------------------------------------------------------------------
    // /feedback
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "feedback".to_string(),
        func: Arc::new(|_soul: &mut KimiSoul, args: &str| {
            Box::pin(async move {
                let text = args.trim();
                if text.is_empty() {
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                        text: "Spot a bug or have feedback? Visit https://github.com/MoonshotAI/kimi-cli/issues".to_string(),
                    }));
                    return;
                }
                // In a full implementation, this would POST to the platform feedback API.
                // For now, print the GitHub issues URL as fallback.
                crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                    text: "Feedback submission via API is not yet implemented. Please visit https://github.com/MoonshotAI/kimi-cli/issues".to_string(),
                }));
            })
        }),
        description: "Submit feedback to make Kimi Code CLI better".to_string(),
        aliases: Vec::new(),
    });

    // -------------------------------------------------------------------------
    // /new
    // Python: `@registry.command async def new(...)`
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "new".to_string(),
        func: Arc::new(|soul: &mut KimiSoul, _args: &str| {
            Box::pin(async move {
                if soul.session.is_empty() {
                    let _ = soul.session.delete().await;
                }
                let work_dir = soul.session.work_dir.clone();
                match crate::session::Session::create(&work_dir, None).await {
                    Ok(new_session) => {
                        let id = new_session.id.clone();
                        crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                            text: format!(
                                "New session created: {}. Restart octopus to switch to it.",
                                id
                            ),
                        }));
                    }
                    Err(e) => {
                        crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                            text: format!("Failed to create new session: {}", e),
                        }));
                    }
                }
            })
        }),
        description: "Start a new session".to_string(),
        aliases: Vec::new(),
    });

    // -------------------------------------------------------------------------
    // /title  (aliases: /rename)
    // Python: `@registry.command(name="title", aliases=["rename"])`
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "title".to_string(),
        func: Arc::new(|soul: &mut KimiSoul, args: &str| {
            Box::pin(async move {
                let new_title = args.trim();
                if new_title.is_empty() {
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                        text: format!("Session title: {}", soul.session.title),
                    }));
                    return;
                }
                let trimmed = new_title.chars().take(200).collect::<String>();
                let session_dir = soul.session.dir();
                let mut fresh = crate::session_state::load_session_state(&session_dir);
                fresh.custom_title = Some(trimmed.clone());
                fresh.title_generated = true;
                crate::session_state::save_session_state(&fresh, &session_dir).ok();
                soul.session.state.custom_title = Some(trimmed.clone());
                soul.session.state.title_generated = true;
                soul.session.title = trimmed.clone();
                crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                    text: format!("Session title set to: {}", trimmed),
                }));
            })
        }),
        description: "Set or show the session title".to_string(),
        aliases: vec!["rename".to_string()],
    });

    // -------------------------------------------------------------------------
    // /sessions  (aliases: /resume)
    // Python: `@registry.command(name="sessions", aliases=["resume"])`
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "sessions".to_string(),
        func: Arc::new(|soul: &mut KimiSoul, _args: &str| {
            Box::pin(async move {
                let work_dir = soul.session.work_dir.clone();
                let sessions = crate::session::Session::list(&work_dir).await;
                if sessions.is_empty() {
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                        text: "No sessions found.".to_string(),
                    }));
                    return;
                }
                let mut lines = vec!["Sessions:".to_string(), String::new()];
                for s in sessions {
                    let marker = if s.id == soul.session.id {
                        " ← current"
                    } else {
                        ""
                    };
                    lines.push(format!(
                        "  {} - {}{}",
                        &s.id[..8.min(s.id.len())],
                        s.title,
                        marker
                    ));
                }
                crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                    text: lines.join("\n"),
                }));
            })
        }),
        description: "List sessions and resume optionally".to_string(),
        aliases: vec!["resume".to_string()],
    });

    // -------------------------------------------------------------------------
    // /web
    // Python shell-level command. Placeholder until web UI is wired.
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "web".to_string(),
        func: Arc::new(|_soul: &mut KimiSoul, _args: &str| {
            Box::pin(async move {
                crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                    text: "Web UI is not yet implemented in octopus-cli.".to_string(),
                }));
            })
        }),
        description: "Open Kimi Code Web UI in browser".to_string(),
        aliases: Vec::new(),
    });

    // -------------------------------------------------------------------------
    // /vis
    // Python shell-level command. Placeholder until visualizer is wired.
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "vis".to_string(),
        func: Arc::new(|_soul: &mut KimiSoul, _args: &str| {
            Box::pin(async move {
                crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                    text: "Visualizer is not yet implemented in octopus-cli.".to_string(),
                }));
            })
        }),
        description: "Open Kimi Agent Tracing Visualizer in browser".to_string(),
        aliases: Vec::new(),
    });

    // -------------------------------------------------------------------------
    // /mcp
    // Python: `@registry.command async def mcp(...)`
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "mcp".to_string(),
        func: Arc::new(|soul: &mut KimiSoul, _args: &str| {
            Box::pin(async move {
                if let Some(ref mcp) = soul.status_snapshot().await.mcp_status {
                    let mut lines = vec!["MCP Servers:".to_string(), String::new()];
                    for server in &mcp.servers {
                        lines.push(format!(
                            "  {} - {} ({} tools)",
                            server.name,
                            server.status,
                            server.tools.len()
                        ));
                    }
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                        text: lines.join("\n"),
                    }));
                } else {
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                        text: "No MCP servers configured.".to_string(),
                    }));
                }
            })
        }),
        description: "Show MCP servers and tools".to_string(),
        aliases: Vec::new(),
    });

    // -------------------------------------------------------------------------
    // /hooks
    // Python: `@registry.command def hooks(...)`
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "hooks".to_string(),
        func: Arc::new(|soul: &mut KimiSoul, _args: &str| {
            Box::pin(async move {
                if soul.config.hooks.is_empty() {
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                        text: "No hooks configured. Add [[hooks]] sections to your config.toml."
                            .to_string(),
                    }));
                    return;
                }
                let mut lines = vec!["Configured hooks:".to_string(), String::new()];
                for hook in &soul.config.hooks {
                    let matcher = hook.matcher.as_deref().unwrap_or("*");
                    lines.push(format!(
                        "  event: {}  matcher: {}  command: {}",
                        hook.event, matcher, hook.command
                    ));
                }
                crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                    text: lines.join("\n"),
                }));
            })
        }),
        description: "List configured hooks".to_string(),
        aliases: Vec::new(),
    });

    // -------------------------------------------------------------------------
    // /undo <turn_number>
    // Python: `@registry.command async def undo(...)`
    // Forks the session at a previous turn so the user can retry.
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "undo".to_string(),
        func: Arc::new(|soul: &mut KimiSoul, args: &str| {
            Box::pin(async move {
                let turns = enumerate_turns(&soul.session.wire_file_path);
                if turns.is_empty() {
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                        text: "No turns found in this session.".to_string(),
                    }));
                    return;
                }

                let turn_idx = if args.trim().is_empty() {
                    turns.len().saturating_sub(1)
                } else {
                    match args.trim().parse::<usize>() {
                        Ok(n) if n > 0 && n <= turns.len() => n - 1,
                        _ => {
                            let mut lines = vec!["Available turns:".to_string()];
                            for (i, (_, text)) in turns.iter().enumerate() {
                                lines.push(format!("  {}. {}", i + 1, text));
                            }
                            lines.push("Usage: /undo <turn_number>".to_string());
                            crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                                text: lines.join("\n"),
                            }));
                            return;
                        }
                    }
                };

                let (wire_line, _user_text) = &turns[turn_idx];
                let work_dir = soul.session.work_dir.clone();

                // If turn 0 selected, create empty session; else fork up to previous turn
                let result = if turn_idx == 0 {
                    match crate::session::Session::create(&work_dir, None).await {
                        Ok(new_session) => {
                            let new_dir = new_session.dir();
                            let mut state = crate::session_state::load_session_state(&new_dir);
                            state.custom_title = Some(format!("Undo: {}", soul.session.title));
                            state.title_generated = true;
                            crate::session_state::save_session_state(&state, &new_dir).ok();
                            Ok(new_session.id)
                        }
                        Err(e) => Err(e),
                    }
                } else {
                    fork_session(&soul.session, &work_dir, Some(wire_line.saturating_sub(1)), "Undo").await
                };

                match result {
                    Ok(new_id) => {
                        crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                            text: format!(
                                "Undone to turn {}. New session: {}. Restart with --session {} to switch.",
                                turn_idx + 1,
                                new_id,
                                new_id
                            ),
                        }));
                    }
                    Err(e) => {
                        crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                            text: format!("Undo failed: {}", e),
                        }));
                    }
                }
            })
        }),
        description: "Undo: fork the session at a previous turn and retry".to_string(),
        aliases: Vec::new(),
    });

    // -------------------------------------------------------------------------
    // /fork
    // Python: `@registry.command async def fork(...)`
    // Copies all history to a new session.
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "fork".to_string(),
        func: Arc::new(|soul: &mut KimiSoul, _args: &str| {
            Box::pin(async move {
                let work_dir = soul.session.work_dir.clone();
                match fork_session(&soul.session, &work_dir, None, "Fork").await {
                    Ok(new_id) => {
                        crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                            text: format!(
                                "Forked session: {}. Restart with --session {} to switch.",
                                new_id, new_id
                            ),
                        }));
                    }
                    Err(e) => {
                        crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                            text: format!("Fork failed: {}", e),
                        }));
                    }
                }
            })
        }),
        description: "Fork the current session (copy all history to a new session)".to_string(),
        aliases: Vec::new(),
    });

    // -------------------------------------------------------------------------
    // /btw
    // Python: `@registry.command async def btw(...)`
    // Side questions without interrupting the main conversation.
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "btw".to_string(),
        func: Arc::new(|soul: &mut KimiSoul, args: &str| {
            Box::pin(async move {
                let question = args.trim();
                if question.is_empty() {
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                        text: "Usage: /btw <question>".to_string(),
                    }));
                    return;
                }

                crate::wire::wire_send(crate::wire::WireEvent::BtwBegin(BtwBegin {
                    question: question.to_string(),
                }));

                let Some(ref llm) = soul.llm else {
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                        text: "No LLM configured. Set a model first.".to_string(),
                    }));
                    crate::wire::wire_send(crate::wire::WireEvent::BtwEnd(BtwEnd {}));
                    return;
                };

                let messages = vec![crate::wire::Message {
                    role: "user".to_string(),
                    content: vec![crate::wire::ContentPart::Text {
                        text: question.to_string(),
                    }],
                    tool_call_id: None,
                    tool_calls: None,
                }];

                match llm
                    .complete(
                        Some("You are a helpful assistant. Answer the user's question concisely."),
                        &messages,
                        None,
                    )
                    .await
                {
                    Ok(result) => {
                        let answer = result.message.extract_text("\n");
                        crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                            text: answer,
                        }));
                    }
                    Err(e) => {
                        crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                            text: format!("Side question failed: {}", e),
                        }));
                    }
                }

                crate::wire::wire_send(crate::wire::WireEvent::BtwEnd(BtwEnd {}));
            })
        }),
        description: "Ask a side question without interrupting the main conversation".to_string(),
        aliases: Vec::new(),
    });

    // -------------------------------------------------------------------------
    // /editor <command>
    // Python: `@registry.command async def editor(...)`
    // Sets the default external editor used for Ctrl-O.
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "editor".to_string(),
        func: Arc::new(|soul: &mut KimiSoul, args: &str| {
            Box::pin(async move {
                let arg = args.trim();
                if arg.is_empty() {
                    let current = if soul.config.default_editor.is_empty() {
                        std::env::var("EDITOR")
                            .or_else(|_| std::env::var("VISUAL"))
                            .unwrap_or_else(|_| "auto-detect".to_string())
                    } else {
                        soul.config.default_editor.clone()
                    };
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                        text: format!("Current editor: {}. Usage: /editor <command>", current),
                    }));
                    return;
                }

                // Validate binary exists
                let binary = arg.split_whitespace().next().unwrap_or(arg);
                let in_path = if std::path::PathBuf::from(binary).is_absolute() {
                    std::path::PathBuf::from(binary).exists()
                } else {
                    std::env::var("PATH").ok().is_some_and(|path_env| {
                        path_env
                            .split(':')
                            .any(|dir| std::path::PathBuf::from(dir).join(binary).exists())
                    })
                };

                if !in_path {
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                        text: format!("Warning: '{}' not found in PATH. Setting anyway.", binary),
                    }));
                }

                soul.config.default_editor = arg.to_string();
                if let Some(ref source) = soul.config.source_file {
                    let _ = crate::config::save_config(&soul.config, Some(source));
                }
                crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                    text: format!("Editor set to: {}. Restart to apply.", arg),
                }));
            })
        }),
        description: "Set default external editor for Ctrl-O".to_string(),
        aliases: Vec::new(),
    });

    // -------------------------------------------------------------------------
    // /copy [all | last]
    // Copy the last assistant message (or entire conversation) to clipboard.
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "copy".to_string(),
        func: Arc::new(|soul: &mut KimiSoul, args: &str| {
            Box::pin(async move {
                let arg = args.trim();
                let history = soul.context.lock().await.history().to_vec();
                let text = if arg == "all" {
                    // Copy entire conversation context
                    history
                        .iter()
                        .map(|m| {
                            let role = &m.role;
                            let content = m.extract_text("\n");
                            format!("{}: {}", role, content)
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n")
                } else {
                    // Copy last assistant message
                    let last = history
                        .iter()
                        .rev()
                        .find(|m| m.role == "assistant")
                        .map(|m| m.extract_text("\n"));
                    match last {
                        Some(content) => content,
                        None => {
                            crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                                text: "No assistant message found to copy.".to_string(),
                            }));
                            return;
                        }
                    }
                };

                match crate::utils::clipboard::copy_text(&text) {
                    Ok(()) => {
                        crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                            text: if arg == "all" {
                                "Copied entire conversation to clipboard.".to_string()
                            } else {
                                "Copied last assistant message to clipboard.".to_string()
                            },
                        }));
                    }
                    Err(e) => {
                        crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                            text: format!("Failed to copy to clipboard: {}", e),
                        }));
                    }
                }
            })
        }),
        description:
            "Copy last assistant message (or /copy all for entire conversation) to clipboard"
                .to_string(),
        aliases: Vec::new(),
    });

    // -------------------------------------------------------------------------
    // /task
    // Python shell-level command. Placeholder for background task browser.
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "task".to_string(),
        func: Arc::new(|_soul: &mut KimiSoul, _args: &str| {
            Box::pin(async move {
                crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                    text: "Background task browser is not yet implemented in octopus-cli."
                        .to_string(),
                }));
            })
        }),
        description: "Browse and manage background tasks".to_string(),
        aliases: Vec::new(),
    });

    // -------------------------------------------------------------------------
    // /theme dark|light
    // Python: `@registry.command def theme(...)`
    // -------------------------------------------------------------------------
    registry.register(SlashCommand {
        name: "theme".to_string(),
        func: Arc::new(|soul: &mut KimiSoul, args: &str| {
            Box::pin(async move {
                let arg = args.trim().to_lowercase();
                if arg.is_empty() {
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                        text: format!(
                            "Current theme: {}. Usage: /theme dark | /theme light",
                            soul.config.theme
                        ),
                    }));
                    return;
                }
                if arg != "dark" && arg != "light" {
                    crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                        text: "Unknown theme. Use 'dark' or 'light'.".to_string(),
                    }));
                    return;
                }
                soul.config.theme = arg.clone();
                crate::wire::wire_send(crate::wire::WireEvent::TextPart(TextPart {
                    text: format!("Theme set to: {}. Restart to apply.", arg),
                }));
            })
        }),
        description: "Switch terminal color theme (dark/light)".to_string(),
        aliases: Vec::new(),
    });

    registry
}

// =============================================================================
// Helper functions
// =============================================================================
// These helpers mirror private utility functions in the Python soul/slash.py
// and session_fork.py modules.
// =============================================================================

/// Fork a session, optionally truncating history at a given turn index.
///
/// Mirrors Python's `fork_session()` in `kimi_cli/session_fork.py`.
/// Copies `wire.jsonl` (optionally truncated) and `context.jsonl` into a
/// newly created session directory, then sets a custom title.
async fn fork_session(
    source: &crate::session::Session,
    work_dir: &std::path::Path,
    turn_index: Option<usize>,
    title_prefix: &str,
) -> std::io::Result<String> {
    use crate::session_state::{load_session_state, save_session_state};

    let source_dir = source.dir();
    let wire_src = source_dir.join("wire.jsonl");
    let context_src = source_dir.join("context.jsonl");

    let new_session = crate::session::Session::create(work_dir, None).await?;
    let new_dir = new_session.dir();

    if wire_src.exists() {
        let content = tokio::fs::read_to_string(&wire_src).await?;
        let lines: Vec<&str> = content.lines().collect();
        let to_write = if let Some(idx) = turn_index {
            // Keep history up to turn_index (inclusive)
            lines
                .into_iter()
                .take(idx + 1)
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            content
        };
        tokio::fs::write(new_dir.join("wire.jsonl"), to_write).await?;
    }
    if context_src.exists() {
        let content = tokio::fs::read_to_string(&context_src).await?;
        tokio::fs::write(new_dir.join("context.jsonl"), content).await?;
    }

    let mut state = load_session_state(&new_dir);
    state.custom_title = Some(format!("{}: {}", title_prefix, source.title));
    state.title_generated = true;
    save_session_state(&state, &new_dir).ok();

    Ok(new_session.id)
}

/// Enumerate user turns from a wire file.
///
/// Mirrors Python's `enumerate_turns()` in `kimi_cli/session_fork.py`.
/// Returns a vec of `(line_number, truncated_user_text)` for every wire line
/// that contains a `"user_input"` key.
fn enumerate_turns(wire_path: &std::path::Path) -> Vec<(usize, String)> {
    let mut turns = Vec::new();
    if let Ok(content) = std::fs::read_to_string(wire_path) {
        for (i, line) in content.lines().enumerate() {
            if let Ok(obj) = serde_json::from_str::<serde_json::Value>(line)
                && obj.get("user_input").is_some()
            {
                let text = obj["user_input"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(60)
                    .collect::<String>();
                turns.push((i, text));
            }
        }
    }
    turns
}

// =============================================================================
// Slash command parsing
// =============================================================================
// Mirrors `parse_slash_command_call()` in `kimi_cli/utils/slashcmd.py`.
// =============================================================================

/// Parse a leading `/command args...` from user input.
///
/// Returns `None` if the text does not start with `/` or has no command name.
/// This is the Rust equivalent of Python's `parse_slash_command_call()`.
pub fn parse_slash_command_call(text: &str) -> Option<SlashCommandCall> {
    let text = text.trim();
    if !text.starts_with('/') {
        return None;
    }
    let rest = &text[1..];
    let mut parts = rest.splitn(2, ' ');
    let name = parts.next()?.trim();
    let args = parts.next().unwrap_or("").trim();
    if name.is_empty() {
        return None;
    }
    Some(SlashCommandCall {
        name: name.to_string(),
        args: args.to_string(),
    })
}

/// The parsed result of `parse_slash_command_call`.
///
/// Mirrors Python's `SlashCommandCall` dataclass (without `raw_input`, which
/// can be reconstructed from `name` and `args` when needed).
pub struct SlashCommandCall {
    pub name: String,
    pub args: String,
}
