use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use chrono::DateTime;
use chrono::Duration;
use chrono::NaiveTime;
use chrono::Timelike;
use chrono::Utc;
use codex_protocol::ThreadId;
use crossterm::event::KeyCode;
use ratatui::text::Line;
use serde::Deserialize;
use serde::Serialize;

use crate::app::App;
use crate::app_event::AppEvent;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionShortcutAction;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::custom_prompt_view::CustomPromptSubmitMode;
use crate::bottom_pane::custom_prompt_view::CustomPromptView;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;
use crate::claude_panes::CODEX_MAIN_PANE_ID;
use crate::claude_panes::ClaudePaneStatus;
use crate::key_hint;
use crate::spawn_orchestration::SpawnTaskTarget;
use crate::spawn_orchestration::node_id_pane;
use crate::spawn_orchestration::node_id_thread;
use crate::spawn_orchestration::pane_node_id;
use crate::spawn_orchestration::thread_node_id;

const ORCHESTRATE_FENCE_OPEN: &str = "```pfterminal-orchestrate";
const ORCHESTRATE_FENCE_CLOSE: &str = "```";
const DEFAULT_EXPIRY_SECONDS: i64 = 4 * 60 * 60;
const DEFAULT_MAX_FIRES: u32 = 20;
const DEFAULT_COOLDOWN_S: u64 = 60;
const DEFAULT_ASSIGNMENT_CADENCE_S: u64 = 15 * 60;
const MIN_ASSIGNMENT_COMPLETION_INTERVAL_S: i64 = 3;
pub(crate) const DRAFT_WITH_MANAGER_SPEC: &str = "draft-with-manager";
const DEFAULT_STOP_MARKER: &str = "WHIP_DONE";
const BUILTIN_KEEP_GOING_WHIP: &str = "keep-going";
const BUILTIN_KEEP_GOING_PATH: &str = "built-in:keep-going";
const BUILTIN_KEEP_GOING_INSTRUCTION: &str =
    "Continue the assigned work. If finished, report done and emit WHIP_DONE.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum WhipMode {
    Review,
    Auto,
}

impl WhipMode {
    fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "review" => Ok(Self::Review),
            "auto" => Ok(Self::Auto),
            other => Err(format!(
                "Unknown whip mode `{other}`; expected review or auto."
            )),
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Auto => "auto",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum WhipState {
    #[default]
    Armed,
    Paused,
    Exhausted,
    Expired,
    Detached,
}

impl WhipState {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Armed => "armed",
            Self::Paused => "paused",
            Self::Exhausted => "exhausted",
            Self::Expired => "expired",
            Self::Detached => "detached",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AssignmentPhase {
    Drafting,
    Executing,
    Blocked { reason: String },
    Done,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum WhipKind {
    #[default]
    LegacyNudge,
    Assignment {
        phase: AssignmentPhase,
        execution_started_utc: Option<DateTime<Utc>>,
        last_user_turn_utc: Option<DateTime<Utc>>,
        failure_backoff_level: u8,
        execution_duration_s: Option<i64>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Whip {
    pub(crate) id: String,
    pub(crate) holder: Option<String>,
    pub(crate) target: String,
    pub(crate) instructions: String,
    pub(crate) mode: WhipMode,
    #[serde(default)]
    pub(crate) kind: WhipKind,
    pub(crate) expires_at: Option<DateTime<Utc>>,
    pub(crate) max_fires: u32,
    pub(crate) cooldown_s: u64,
    pub(crate) stop_marker: String,
    #[serde(default)]
    pub(crate) fires: u32,
    #[serde(default)]
    pub(crate) last_fire_utc: Option<DateTime<Utc>>,
    #[serde(default)]
    pub(crate) state: WhipState,
    #[serde(default)]
    pub(crate) last_idle_generation_fired: Option<u64>,
    #[serde(default)]
    pub(crate) empty_output_fires: u32,
    #[serde(default)]
    pub(crate) consecutive_failed_turns: u32,
    #[serde(default)]
    pub(crate) assignment_unreachable_since_utc: Option<DateTime<Utc>>,
    #[serde(default)]
    pub(crate) pending_review_fire: Option<u32>,
    #[serde(default)]
    pub(crate) ignored_review_fires: u32,
    #[serde(default)]
    pub(crate) expiry_notified: bool,
    #[serde(default)]
    pub(crate) last_target_output: Option<String>,
    #[serde(default)]
    pub(crate) last_dispatch_result: Option<String>,
}

impl Whip {
    fn new(
        id: String,
        holder: Option<String>,
        target: String,
        instructions: String,
        options: ResolvedAttachOptions,
        now: DateTime<Utc>,
    ) -> Self {
        let expires_at = options
            .expiry
            .unwrap_or_else(|| Some(now + Duration::seconds(DEFAULT_EXPIRY_SECONDS)));
        Self {
            id,
            holder,
            target,
            instructions,
            mode: options.mode.unwrap_or(WhipMode::Review),
            kind: WhipKind::LegacyNudge,
            expires_at,
            max_fires: options.max_fires.unwrap_or(DEFAULT_MAX_FIRES),
            cooldown_s: options.cooldown_s.unwrap_or(DEFAULT_COOLDOWN_S),
            stop_marker: options
                .stop_marker
                .unwrap_or_else(|| DEFAULT_STOP_MARKER.to_string()),
            fires: 0,
            last_fire_utc: None,
            state: WhipState::Armed,
            last_idle_generation_fired: None,
            empty_output_fires: 0,
            consecutive_failed_turns: 0,
            assignment_unreachable_since_utc: None,
            pending_review_fire: None,
            ignored_review_fires: 0,
            expiry_notified: false,
            last_target_output: None,
            last_dispatch_result: None,
        }
    }

    fn is_armed(&self) -> bool {
        self.state == WhipState::Armed
    }

    fn is_assignment(&self) -> bool {
        matches!(self.kind, WhipKind::Assignment { .. })
    }
}

#[derive(Debug, Clone, Default)]
struct ResolvedAttachOptions {
    mode: Option<WhipMode>,
    expiry: Option<Option<DateTime<Utc>>>,
    max_fires: Option<u32>,
    cooldown_s: Option<u64>,
    stop_marker: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HolderArg {
    Me,
    None,
    Target(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OrchestrateCommand {
    Status,
    Attach {
        target: String,
        whip_name: String,
        mode: Option<WhipMode>,
        expiry: Option<ExpiryArg>,
        max_fires: Option<u32>,
        cooldown_s: Option<u64>,
        holder: Option<HolderArg>,
    },
    Detach(String),
    Pause(String),
    Resume(String),
    Start(String),
    Extend {
        id: String,
        duration: DurationArg,
    },
    Fire(String),
    Test(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExpiryArg {
    Duration(DurationArg),
    UntilTodayOrTomorrow { hour: u32, minute: u32 },
    Unlimited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DurationArg {
    seconds: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrchestrateBlock {
    pub(crate) command: OrchestrateCommand,
}

#[derive(Debug, Clone)]
struct WhipDocDefaults {
    mode: Option<WhipMode>,
    max_fires: Option<u32>,
    cooldown_s: Option<u64>,
    stop_marker: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WhipInstructionEntry {
    name: String,
    description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OrchestrateTargetEntry {
    target: String,
    node_id: String,
    name: String,
    description: String,
    is_current: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FireDestination {
    Native(ThreadId),
    ClaudePane(String),
}

#[derive(Debug, Clone)]
struct FirePlan {
    whip_id: String,
    mode: WhipMode,
    destination: FireDestination,
    task: String,
    target_idle_generation: u64,
    destination_label: String,
}

pub(crate) fn parse_orchestrate_command(input: &str) -> Result<OrchestrateCommand, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(OrchestrateCommand::Status);
    }
    let mut parts = trimmed.split_whitespace();
    let action = parts.next().unwrap_or_default().to_ascii_lowercase();
    match action.as_str() {
        "status" => Ok(OrchestrateCommand::Status),
        "attach" => {
            let target = parts
                .next()
                .ok_or_else(|| orchestrate_usage().to_string())?
                .to_string();
            let whip_name = parts
                .next()
                .ok_or_else(|| orchestrate_usage().to_string())?
                .to_string();
            let mut mode = None;
            let mut expiry = None;
            let mut max_fires = None;
            let mut cooldown_s = None;
            let mut holder = None;
            while let Some(flag) = parts.next() {
                match flag {
                    "--mode" => {
                        let value = parts
                            .next()
                            .ok_or_else(|| "Missing value after --mode.".to_string())?;
                        mode = Some(WhipMode::parse(value)?);
                    }
                    "--for" => {
                        let value = parts
                            .next()
                            .ok_or_else(|| "Missing value after --for.".to_string())?;
                        expiry = Some(if value.eq_ignore_ascii_case("unlimited") {
                            ExpiryArg::Unlimited
                        } else {
                            ExpiryArg::Duration(parse_duration_arg(value)?)
                        });
                    }
                    "--until" => {
                        let value = parts
                            .next()
                            .ok_or_else(|| "Missing value after --until.".to_string())?;
                        expiry = Some(parse_until_arg(value)?);
                    }
                    "--max" => {
                        let value = parts
                            .next()
                            .ok_or_else(|| "Missing value after --max.".to_string())?;
                        max_fires = Some(parse_positive_u32(value, "--max")?);
                    }
                    "--cooldown" => {
                        let value = parts
                            .next()
                            .ok_or_else(|| "Missing value after --cooldown.".to_string())?;
                        cooldown_s = Some(parse_duration_arg(value)?.seconds.max(0) as u64);
                    }
                    "--holder" => {
                        let value = parts
                            .next()
                            .ok_or_else(|| "Missing value after --holder.".to_string())?;
                        holder = Some(parse_holder_arg(value));
                    }
                    other => return Err(format!("Unknown /orchestrate attach option `{other}`.")),
                }
            }
            Ok(OrchestrateCommand::Attach {
                target,
                whip_name,
                mode,
                expiry,
                max_fires,
                cooldown_s,
                holder,
            })
        }
        "detach" => one_arg(action.as_str(), parts).map(OrchestrateCommand::Detach),
        "pause" => one_arg(action.as_str(), parts).map(OrchestrateCommand::Pause),
        "resume" => one_arg(action.as_str(), parts).map(OrchestrateCommand::Resume),
        "start" => one_arg(action.as_str(), parts).map(OrchestrateCommand::Start),
        "fire" => one_arg(action.as_str(), parts).map(OrchestrateCommand::Fire),
        "test" => one_arg(action.as_str(), parts).map(OrchestrateCommand::Test),
        "extend" => {
            let id = parts
                .next()
                .ok_or_else(|| "Usage: /orchestrate extend <id> <duration>".to_string())?
                .to_string();
            let duration = parts
                .next()
                .ok_or_else(|| "Usage: /orchestrate extend <id> <duration>".to_string())
                .and_then(parse_duration_arg)?;
            if parts.next().is_some() {
                return Err("Usage: /orchestrate extend <id> <duration>".to_string());
            }
            Ok(OrchestrateCommand::Extend { id, duration })
        }
        _ => Err(orchestrate_usage().to_string()),
    }
}

pub(crate) fn extract_orchestrate_blocks(text: &str) -> (String, Vec<OrchestrateBlock>) {
    let mut visible = String::new();
    let mut blocks = Vec::new();
    let mut rest = text;
    while let Some(start_index) = rest.find(ORCHESTRATE_FENCE_OPEN) {
        visible.push_str(&rest[..start_index]);
        let block = &rest[start_index..];
        let Some(header_end) = block.find('\n') else {
            visible.push_str(block);
            rest = "";
            break;
        };
        let content_start = header_end + 1;
        let Some(close_index) = block[content_start..].find(ORCHESTRATE_FENCE_CLOSE) else {
            visible.push_str(block);
            rest = "";
            break;
        };
        let content_end = content_start + close_index;
        let content = &block[content_start..content_end];
        if let Some(command) = parse_orchestrate_block_content(content) {
            blocks.push(OrchestrateBlock { command });
        }
        rest = &block[content_end + ORCHESTRATE_FENCE_CLOSE.len()..];
    }
    visible.push_str(rest);
    (visible.trim().to_string(), blocks)
}

fn parse_orchestrate_block_content(content: &str) -> Option<OrchestrateCommand> {
    let fields = yamlish_fields(content);
    let action = fields.get("action")?.trim().to_ascii_lowercase();
    match action.as_str() {
        "attach" => {
            let target = fields.get("target")?.trim().to_string();
            let whip_name = fields
                .get("whip")
                .or_else(|| fields.get("whip_name"))?
                .trim()
                .to_string();
            let mode = fields
                .get("mode")
                .map(|value| WhipMode::parse(value))
                .transpose()
                .ok()?;
            let holder = fields.get("holder").map(|value| parse_holder_arg(value));
            let expiry = fields
                .get("for")
                .map(|value| {
                    if value.trim().eq_ignore_ascii_case("unlimited") {
                        Ok(ExpiryArg::Unlimited)
                    } else {
                        parse_duration_arg(value).map(ExpiryArg::Duration)
                    }
                })
                .or_else(|| {
                    fields
                        .get("until")
                        .map(|value| parse_until_arg(value.trim()))
                })
                .transpose()
                .ok()?;
            Some(OrchestrateCommand::Attach {
                target,
                whip_name,
                mode,
                expiry,
                max_fires: fields
                    .get("max")
                    .and_then(|value| value.trim().parse::<u32>().ok()),
                cooldown_s: fields.get("cooldown").and_then(|value| {
                    parse_duration_arg(value)
                        .ok()
                        .map(|duration| duration.seconds.max(0) as u64)
                }),
                holder,
            })
        }
        "detach" | "pause" | "resume" | "fire" | "test" => {
            let id = fields
                .get("id")
                .or_else(|| fields.get("target"))
                .or_else(|| fields.get("whip"))?
                .trim()
                .to_string();
            match action.as_str() {
                "detach" => Some(OrchestrateCommand::Detach(id)),
                "pause" => Some(OrchestrateCommand::Pause(id)),
                "resume" => Some(OrchestrateCommand::Resume(id)),
                "start" => Some(OrchestrateCommand::Start(id)),
                "fire" => Some(OrchestrateCommand::Fire(id)),
                "test" => Some(OrchestrateCommand::Test(id)),
                _ => None,
            }
        }
        "extend" => {
            let id = fields.get("id")?.trim().to_string();
            let duration = fields
                .get("duration")
                .and_then(|value| parse_duration_arg(value).ok())?;
            Some(OrchestrateCommand::Extend { id, duration })
        }
        _ => None,
    }
}

pub(crate) fn orchestrate_usage() -> &'static str {
    "Usage: /orchestrate [status|attach <target> <whip-name> [--mode review|auto] [--for 4h|--until HH:MM|--for unlimited] [--max N] [--cooldown S] [--holder me|none]|detach <id|target>|pause <id>|resume <id>|extend <id> <duration>|fire <id>|test <id>]"
}

pub(crate) fn format_whip_status(whips: &HashMap<String, Whip>, now: DateTime<Utc>) -> String {
    if whips.is_empty() {
        return "No assignments or legacy automation are active.".to_string();
    }
    let mut ordered: Vec<_> = whips.values().collect();
    ordered.sort_by(|a, b| a.id.cmp(&b.id));
    let mut out = String::from("Assignments and legacy automation:\n");
    for whip in ordered {
        let manager = whip.holder.as_deref().unwrap_or("none");
        let expiry = match whip.expires_at {
            Some(expires_at) if expires_at <= now => "expired".to_string(),
            Some(expires_at) => format!("expires {}", expires_at.format("%H:%MZ")),
            None => "unlimited".to_string(),
        };
        match &whip.kind {
            WhipKind::Assignment { phase, .. } => {
                let _ = writeln!(
                    out,
                    "- Assignment {}: Manager {} -> Worker {} using {} ({}, {}, {})",
                    whip.id,
                    manager,
                    whip.target,
                    whip.instructions,
                    assignment_phase_label(phase),
                    whip.state.label(),
                    expiry,
                );
            }
            WhipKind::LegacyNudge => {
                let mode = match whip.mode {
                    WhipMode::Review => "manager-led",
                    WhipMode::Auto => "automatic",
                };
                let _ = writeln!(
                    out,
                    "- Legacy automation {}: Manager {} -> Worker {} using {} ({}, {}/{}, {}, {})",
                    whip.id,
                    manager,
                    whip.target,
                    whip.instructions,
                    mode,
                    whip.fires,
                    whip.max_fires,
                    whip.state.label(),
                    expiry,
                );
            }
        }
    }
    out.trim_end().to_string()
}

pub(crate) fn resolve_whip_instruction_path(
    codex_home: &Path,
    cwd: &Path,
    name: &str,
) -> Result<Option<PathBuf>, String> {
    validate_whip_name(name)?;
    let file_name = if name.ends_with(".md") {
        name.to_string()
    } else {
        format!("{name}.md")
    };
    for product_dir in [".corbanu", ".pfterminal"] {
        let project_path = cwd.join(product_dir).join("whips").join(&file_name);
        if project_path.exists() {
            return Ok(Some(project_path));
        }
    }
    let global_path = codex_home.join("whips").join(file_name);
    Ok(global_path.exists().then_some(global_path))
}

fn normalize_whip_name(name: &str) -> &str {
    name.trim()
        .strip_suffix(".md")
        .unwrap_or_else(|| name.trim())
}

fn is_builtin_keep_going_name(name: &str) -> bool {
    normalize_whip_name(name) == BUILTIN_KEEP_GOING_WHIP
}

fn scan_whip_instruction_dir(
    dir: &Path,
    source: &str,
    seen: &mut HashSet<String>,
    entries: &mut Vec<WhipInstructionEntry>,
) {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let Some(name) = file_name.strip_suffix(".md") else {
            continue;
        };
        if validate_whip_name(name).is_err() || !seen.insert(name.to_string()) {
            continue;
        }
        entries.push(WhipInstructionEntry {
            name: name.to_string(),
            description: format!("{source}: {}", path.display()),
        });
    }
}

fn available_whip_instruction_entries(codex_home: &Path, cwd: &Path) -> Vec<WhipInstructionEntry> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    scan_whip_instruction_dir(
        &cwd.join(".corbanu").join("whips"),
        "project",
        &mut seen,
        &mut entries,
    );
    scan_whip_instruction_dir(
        &cwd.join(".pfterminal").join("whips"),
        "legacy project",
        &mut seen,
        &mut entries,
    );
    scan_whip_instruction_dir(&codex_home.join("whips"), "global", &mut seen, &mut entries);
    if seen.insert(BUILTIN_KEEP_GOING_WHIP.to_string()) {
        entries.push(WhipInstructionEntry {
            name: BUILTIN_KEEP_GOING_WHIP.to_string(),
            description: "built-in: continue work until WHIP_DONE".to_string(),
        });
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    entries
}

fn validate_whip_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Whip name cannot be empty.".to_string());
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains("..") {
        return Err(format!(
            "Invalid whip name `{name}`; use a basename from the whips directory."
        ));
    }
    if trimmed.eq_ignore_ascii_case(DRAFT_WITH_MANAGER_SPEC) {
        return Err(format!(
            "`{DRAFT_WITH_MANAGER_SPEC}` is reserved for guided drafting."
        ));
    }
    Ok(())
}

fn read_whip_instruction(
    codex_home: &Path,
    cwd: &Path,
    name: &str,
) -> Result<(PathBuf, String), String> {
    let path = match resolve_whip_instruction_path(codex_home, cwd, name)? {
        Some(path) => path,
        None if is_builtin_keep_going_name(name) => {
            return Ok((
                PathBuf::from(BUILTIN_KEEP_GOING_PATH),
                BUILTIN_KEEP_GOING_INSTRUCTION.to_string(),
            ));
        }
        None => return Err(format!("No whip instruction file found for `{name}`.")),
    };
    let contents = fs::read_to_string(&path)
        .map_err(|err| format!("Failed to read whip `{}`: {err}", path.display()))?;
    if contents.trim().is_empty() {
        return Err(format!(
            "Whip instruction file `{}` is empty.",
            path.display()
        ));
    }
    Ok((path, contents))
}

pub(crate) fn orchestrate_guided_attach_args(
    target: &str,
    duration_arg: &str,
    whip_name: &str,
    manager_node_id: &str,
) -> String {
    let cadence_s = assignment_default_cadence_s();
    format!(
        "attach {target} {whip_name} --mode review --for {duration_arg} --cooldown {cadence_s}s --holder {manager_node_id}"
    )
}

fn assignment_default_cadence_s() -> u64 {
    if std::env::var("PFTERMINAL_ORCHESTRATE_QA").as_deref() == Ok("1") {
        return std::env::var("PFTERMINAL_ORCHESTRATE_TEST_CADENCE_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|seconds| (1..=DEFAULT_ASSIGNMENT_CADENCE_S).contains(seconds))
            .unwrap_or(DEFAULT_ASSIGNMENT_CADENCE_S);
    }
    DEFAULT_ASSIGNMENT_CADENCE_S
}

fn truncate_for_orchestrate_display(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

pub(crate) fn whip_suffix_for_target(
    whips: &HashMap<String, Whip>,
    target_node_id: &str,
) -> String {
    if let Some(assignment) = whips.values().find(|whip| {
        whip.is_assignment()
            && whip.state == WhipState::Armed
            && (whip.target == target_node_id || whip.holder.as_deref() == Some(target_node_id))
    }) {
        let phase = match &assignment.kind {
            WhipKind::Assignment { phase, .. } => assignment_phase_label(phase),
            WhipKind::LegacyNudge => unreachable!(),
        };
        if assignment.target == target_node_id {
            let manager = assignment.holder.as_deref().unwrap_or("missing Manager");
            return format!("; managed-by {manager}; {phase}");
        }
        return format!("; managing {}; {phase}", assignment.target);
    }
    let Some(whip) = whips
        .values()
        .find(|whip| whip.target == target_node_id && whip.state == WhipState::Armed)
    else {
        return String::new();
    };
    let expiry = whip
        .expires_at
        .map(|expires_at| format!(", expires {}", expires_at.format("%H:%MZ")))
        .unwrap_or_else(|| ", unlimited".to_string());
    format!(
        "; whip={}({}, {}/{}{})",
        whip.instructions,
        whip.mode.label(),
        whip.fires,
        whip.max_fires,
        expiry
    )
}

fn orchestrate_shortcut_command(key: char, args: String) -> SelectionShortcutAction {
    SelectionShortcutAction {
        key: key_hint::plain(KeyCode::Char(key)),
        action: Box::new(move |tx| {
            tx.send(AppEvent::HandleOrchestrateCommand { args: args.clone() });
        }),
        dismiss_on_select: true,
    }
}

fn orchestrate_shortcut_extend(key: char, whip_id: String) -> SelectionShortcutAction {
    SelectionShortcutAction {
        key: key_hint::plain(KeyCode::Char(key)),
        action: Box::new(move |tx| {
            tx.send(AppEvent::OpenOrchestrateExtendDurationPicker {
                whip_id: whip_id.clone(),
            });
        }),
        dismiss_on_select: true,
    }
}

fn orchestrate_command_item(name: &str, description: &str, args: String) -> SelectionItem {
    SelectionItem {
        name: name.to_string(),
        description: Some(description.to_string()),
        actions: vec![Box::new(move |tx| {
            tx.send(AppEvent::HandleOrchestrateCommand { args: args.clone() });
        })],
        dismiss_on_select: true,
        ..Default::default()
    }
}

fn suggested_whip_name(instructions: &str) -> String {
    let mut slug = String::new();
    for ch in instructions
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("custom whip")
        .chars()
    {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if (ch.is_whitespace() || ch == '-' || ch == '_') && !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() >= 32 {
            break;
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "custom-whip".to_string()
    } else {
        slug.to_string()
    }
}

fn save_global_whip_instruction(
    codex_home: &Path,
    requested_name: &str,
    instructions: &str,
) -> Result<String, String> {
    validate_whip_name(requested_name)?;
    let base = normalize_whip_name(requested_name).to_string();
    let dir = codex_home.join("whips");
    fs::create_dir_all(&dir)
        .map_err(|err| format!("Failed to create `{}`: {err}", dir.display()))?;
    for suffix in 1..1000 {
        let candidate = if suffix == 1 {
            base.clone()
        } else {
            format!("{base}-{suffix}")
        };
        validate_whip_name(&candidate)?;
        let path = dir.join(format!("{candidate}.md"));
        if path.exists() || is_builtin_keep_going_name(&candidate) {
            continue;
        }
        let mut body = instructions.trim().to_string();
        body.push('\n');
        fs::write(&path, body)
            .map_err(|err| format!("Failed to write whip `{}`: {err}", path.display()))?;
        return Ok(candidate);
    }
    Err(format!("Could not find an unused whip name for `{base}`."))
}

impl App {
    fn orchestrate_now(&self) -> DateTime<Utc> {
        #[cfg(test)]
        if let Some(now) = self.orchestrate_now_override {
            return now;
        }
        Utc::now()
    }

    pub(crate) fn handle_orchestrate_command(&mut self, args: String) {
        let trimmed = args.trim();
        if trimmed.is_empty() {
            self.open_orchestrate_fast_target_picker();
            return;
        }
        if trimmed.eq_ignore_ascii_case("status") {
            self.open_orchestrate_status_view();
            return;
        }
        if trimmed.eq_ignore_ascii_case("attach") {
            self.open_orchestrate_target_picker();
            return;
        }
        match parse_orchestrate_command(&args) {
            Ok(command) => self.apply_orchestrate_command(command, CommandOrigin::User),
            Err(err) => self.chat_widget.add_error_message(err),
        }
    }

    pub(crate) fn attach_guided_assignment(&mut self, args: &str) -> Result<String, String> {
        let OrchestrateCommand::Attach {
            target,
            whip_name,
            mode,
            expiry,
            max_fires,
            cooldown_s,
            holder,
        } = parse_orchestrate_command(args)?
        else {
            return Err("Expected an assignment attach command.".to_string());
        };
        let attached = self.attach_whip(
            target,
            whip_name,
            mode,
            expiry,
            max_fires,
            cooldown_s,
            holder,
            CommandOrigin::User,
        );
        // #613: as for `/orchestrate attach`, paused at once while the kill
        // switch is on.
        if attached.is_ok() {
            self.pause_whips_for_kill_switch();
        }
        attached
    }

    pub(crate) fn open_orchestrate_status_view(&mut self) {
        let now = self.orchestrate_now();
        let mut whips: Vec<_> = self.orchestrate_whips.values().collect();
        whips.sort_by(|left, right| left.id.cmp(&right.id));

        let mut items = Vec::new();
        items.push(SelectionItem {
            name: "+ New assignment".to_string(),
            description: Some(
                "Pick a Worker and Manager; defaults to 8 hours and Draft with Manager."
                    .to_string(),
            ),
            display_shortcut: Some(key_hint::plain(KeyCode::Char('n'))),
            actions: vec![Box::new(|tx| {
                tx.send(AppEvent::OpenOrchestrateFastTargetPicker);
            })],
            dismiss_on_select: true,
            ..Default::default()
        });

        if whips.is_empty() {
            items.push(SelectionItem {
                name: "No assignments".to_string(),
                description: Some("Use the first row to create one.".to_string()),
                is_disabled: true,
                ..Default::default()
            });
        } else {
            for whip in whips {
                let whip_id = whip.id.clone();
                let pause_resume_args = if whip.state == WhipState::Paused {
                    format!("resume {whip_id}")
                } else {
                    format!("pause {whip_id}")
                };
                let expiry = assignment_expiry_label(whip, now);
                let holder = whip
                    .holder
                    .as_deref()
                    .and_then(|node_id| self.spawn_node_title(node_id))
                    .unwrap_or_else(|| "none".to_string());
                let target = self
                    .spawn_node_title(&whip.target)
                    .unwrap_or_else(|| whip.target.clone());
                let (name, description) = match &whip.kind {
                    WhipKind::Assignment { phase, .. } => {
                        let phase_label = assignment_phase_label(phase);
                        let next =
                            whip.last_fire_utc
                                .map(|last| {
                                    let due = last
                                        + Duration::seconds(
                                            assignment_effective_cadence_s(whip) as i64
                                        );
                                    format!("next mandate {}", due.format("%H:%MZ"))
                                })
                                .unwrap_or_else(|| match phase {
                                    AssignmentPhase::Drafting => "awaiting execution".to_string(),
                                    AssignmentPhase::Executing => {
                                        "awaiting Worker result".to_string()
                                    }
                                    AssignmentPhase::Blocked { .. } => {
                                        "awaiting user decision".to_string()
                                    }
                                    AssignmentPhase::Done => "complete".to_string(),
                                });
                        (
                            format!("Manager {holder} -> Worker {target}"),
                            format!(
                                "{phase_label}; {next}; {expiry}; last dispatch {}; spec {}",
                                whip.last_dispatch_result.as_deref().unwrap_or("none yet"),
                                whip.instructions,
                            ),
                        )
                    }
                    WhipKind::LegacyNudge => (
                        format!("Legacy automation {}: {target}", whip.id),
                        format!(
                            "{}; manager {}; {}; {}/{} runs; {}; {}",
                            whip.state.label(),
                            holder,
                            whip.mode.label(),
                            whip.fires,
                            whip.max_fires,
                            expiry,
                            whip.instructions
                        ),
                    ),
                };
                let mut selected_shortcuts = vec![
                    orchestrate_shortcut_command('d', format!("detach {whip_id}")),
                    orchestrate_shortcut_command('p', pause_resume_args),
                    orchestrate_shortcut_extend('e', whip_id.clone()),
                ];
                if !matches!(
                    whip.kind,
                    WhipKind::Assignment {
                        phase: AssignmentPhase::Drafting,
                        ..
                    }
                ) {
                    selected_shortcuts
                        .push(orchestrate_shortcut_command('f', format!("fire {whip_id}")));
                    selected_shortcuts
                        .push(orchestrate_shortcut_command('t', format!("test {whip_id}")));
                }
                items.push(SelectionItem {
                    name,
                    description: Some(description),
                    search_value: Some(format!(
                        "{} {} {} {}",
                        whip.id, target, holder, whip.instructions
                    )),
                    is_current: whip.state == WhipState::Armed,
                    actions: vec![Box::new({
                        let whip_id = whip_id.clone();
                        move |tx| {
                            tx.send(AppEvent::OpenOrchestrateWhipDetails {
                                whip_id: whip_id.clone(),
                            });
                        }
                    })],
                    selected_shortcuts,
                    dismiss_on_select: false,
                    ..Default::default()
                });
            }
        }

        self.chat_widget.show_selection_view(SelectionViewParams {
            view_id: Some("orchestrate-status"),
            title: Some("Orchestrate".to_string()),
            subtitle: Some("Managers continuously drive Workers through assignments.".to_string()),
            footer_hint: Some(Line::from(
                "Enter details · d detach · p pause/resume · e extend · f mandate · t test · n new assignment",
            )),
            items,
            ..Default::default()
        });
    }

    pub(crate) fn open_orchestrate_fast_target_picker(&mut self) {
        let mut items = self
            .orchestrate_target_entries()
            .into_iter()
            .map(|entry| {
                let target = entry.target.clone();
                SelectionItem {
                    name: entry.name.clone(),
                    description: Some(entry.description.clone()),
                    is_current: entry.is_current,
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::OpenOrchestrateFastManagerPicker {
                            target: target.clone(),
                        });
                    })],
                    dismiss_on_select: true,
                    search_value: Some(format!(
                        "{} {} {}",
                        entry.name, entry.description, entry.target
                    )),
                    ..Default::default()
                }
            })
            .collect::<Vec<_>>();
        if items.is_empty() {
            items.push(SelectionItem {
                name: "No Worker panes available".to_string(),
                description: Some("Create a Corbanu Terminal or Claude pane first.".to_string()),
                is_disabled: true,
                ..Default::default()
            });
        }
        self.chat_widget.show_selection_view(SelectionViewParams {
            view_id: Some("orchestrate-fast-worker"),
            title: Some("New Assignment - Worker".to_string()),
            subtitle: Some(
                "Choose the Worker. The assignment runs for 8 hours after execution starts."
                    .to_string(),
            ),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Search panes".to_string()),
            ..Default::default()
        });
    }

    pub(crate) fn open_orchestrate_fast_manager_picker(&mut self, target: String) {
        let worker_node_id = self.resolve_orchestrate_target_node(&target).ok();
        let mut items = vec![SelectionItem {
            name: "Create Manager pane".to_string(),
            description: Some(format!(
                "Create a Corbanu Terminal pane using {} and start in Drafting.",
                self.native_spawn_default_model()
            )),
            is_default: true,
            actions: vec![Box::new({
                let target = target.clone();
                move |tx| {
                    tx.send(AppEvent::CreateOrchestrateManager {
                        target: target.clone(),
                        duration_arg: "8h".to_string(),
                        whip_name: DRAFT_WITH_MANAGER_SPEC.to_string(),
                    });
                }
            })],
            dismiss_on_select: true,
            ..Default::default()
        }];
        for entry in self.orchestrate_target_entries() {
            if worker_node_id.as_deref() == Some(entry.node_id.as_str())
                || self
                    .primary_thread_id
                    .is_some_and(|thread_id| entry.node_id == thread_node_id(thread_id))
            {
                continue;
            }
            let manager_node_id = entry.node_id.clone();
            items.push(SelectionItem {
                name: format!("Bind {}", entry.name),
                description: Some(format!("{}; starts in Drafting", entry.description)),
                is_current: entry.is_current,
                actions: vec![Box::new({
                    let target = target.clone();
                    move |tx| {
                        tx.send(AppEvent::AttachOrchestrateFastManager {
                            target: target.clone(),
                            manager_node_id: manager_node_id.clone(),
                        });
                    }
                })],
                dismiss_on_select: true,
                search_value: Some(format!("{} {}", entry.name, entry.description)),
                ..Default::default()
            });
        }
        self.chat_widget.show_selection_view(SelectionViewParams {
            view_id: Some("orchestrate-fast-manager"),
            title: Some("New Assignment - Manager".to_string()),
            subtitle: Some(format!(
                "Choose who manages Worker {}. Selecting starts the 8-hour Draft-with-Manager assignment.",
                self.orchestrate_target_label(&target)
            )),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Search panes".to_string()),
            ..Default::default()
        });
    }

    pub(crate) fn open_orchestrate_target_picker(&mut self) {
        let entries = self.orchestrate_target_entries();
        let mut items = Vec::new();
        for entry in entries {
            let target = entry.target.clone();
            items.push(SelectionItem {
                name: entry.name.clone(),
                description: Some(entry.description.clone()),
                is_current: entry.is_current,
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::OpenOrchestrateDurationPicker {
                        target: target.clone(),
                    });
                })],
                dismiss_on_select: true,
                search_value: Some(format!(
                    "{} {} {}",
                    entry.name, entry.description, entry.target
                )),
                ..Default::default()
            });
        }
        if items.is_empty() {
            items.push(SelectionItem {
                name: "No Worker panes available".to_string(),
                description: Some("Create a Corbanu Terminal or Claude pane first.".to_string()),
                is_disabled: true,
                ..Default::default()
            });
        }

        self.chat_widget.show_selection_view(SelectionViewParams {
            view_id: Some("orchestrate-worker"),
            title: Some("New Assignment - Worker".to_string()),
            subtitle: Some(
                "Choose the Worker pane the Manager will continuously drive.".to_string(),
            ),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Search panes".to_string()),
            ..Default::default()
        });
    }

    pub(crate) fn open_orchestrate_duration_picker(&mut self, target: String) {
        let choices = [
            ("15 minutes", "15m", "Short follow-up check."),
            ("1 hour", "1h", "Default short assignment window."),
            ("4 hours", "4h", "Matches the CLI default."),
            ("8 hours", "8h", "Long-running local work."),
            (
                "Unlimited",
                "unlimited",
                "Continues until you pause or end it.",
            ),
        ];
        let items = choices
            .into_iter()
            .map(|(label, duration_arg, description)| {
                let target = target.clone();
                let duration_arg = duration_arg.to_string();
                let duration_label = label.to_string();
                SelectionItem {
                    name: label.to_string(),
                    description: Some(description.to_string()),
                    is_default: duration_arg == "1h",
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::OpenOrchestrateWhipPicker {
                            target: target.clone(),
                            duration_arg: duration_arg.clone(),
                            duration_label: duration_label.clone(),
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();

        self.chat_widget.show_selection_view(SelectionViewParams {
            view_id: Some("orchestrate-duration"),
            title: Some("New Assignment - Duration".to_string()),
            subtitle: Some(format!(
                "Worker: {}",
                self.orchestrate_target_label(&target)
            )),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    pub(crate) fn open_orchestrate_whip_picker(
        &mut self,
        target: String,
        duration_arg: String,
        duration_label: String,
    ) {
        let entries =
            available_whip_instruction_entries(self.config.codex_home.as_ref(), &self.config.cwd);
        let mut items: Vec<SelectionItem> = entries
            .into_iter()
            .map(|entry| {
                let target = target.clone();
                let duration_arg = duration_arg.clone();
                let duration_label = duration_label.clone();
                let whip_name = entry.name.clone();
                SelectionItem {
                    name: entry.name,
                    description: Some(entry.description),
                    is_default: whip_name == BUILTIN_KEEP_GOING_WHIP,
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::OpenOrchestrateManagerPicker {
                            target: target.clone(),
                            duration_arg: duration_arg.clone(),
                            duration_label: duration_label.clone(),
                            whip_name: whip_name.clone(),
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();
        items.insert(
            0,
            SelectionItem {
                name: "Draft with Manager".to_string(),
                description: Some(
                    "Start in Drafting and develop the assignment spec conversationally."
                        .to_string(),
                ),
                is_default: true,
                actions: vec![Box::new({
                    let target = target.clone();
                    let duration_arg = duration_arg.clone();
                    let duration_label = duration_label.clone();
                    move |tx| {
                        tx.send(AppEvent::OpenOrchestrateManagerPicker {
                            target: target.clone(),
                            duration_arg: duration_arg.clone(),
                            duration_label: duration_label.clone(),
                            whip_name: DRAFT_WITH_MANAGER_SPEC.to_string(),
                        });
                    }
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
        );
        items.push(SelectionItem {
            name: "Write new...".to_string(),
            description: Some("Type standing instructions now and save them globally.".to_string()),
            actions: vec![Box::new({
                let target = target.clone();
                let duration_label = duration_label.clone();
                move |tx| {
                    tx.send(AppEvent::OpenOrchestrateWriteWhipPrompt {
                        target: target.clone(),
                        duration_arg: duration_arg.clone(),
                        duration_label: duration_label.clone(),
                    });
                }
            })],
            dismiss_on_select: true,
            ..Default::default()
        });

        self.chat_widget.show_selection_view(SelectionViewParams {
            view_id: Some("orchestrate-spec"),
            title: Some("New Assignment - Spec".to_string()),
            subtitle: Some(format!(
                "{} for {}",
                self.orchestrate_target_label(&target),
                duration_label
            )),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Search specs".to_string()),
            ..Default::default()
        });
    }

    pub(crate) fn open_orchestrate_manager_picker(
        &mut self,
        target: String,
        duration_arg: String,
        duration_label: String,
        whip_name: String,
    ) {
        let worker_node_id = self.resolve_orchestrate_target_node(&target).ok();
        let mut items = Vec::new();
        items.push(SelectionItem {
            name: "Create Manager pane".to_string(),
            description: Some(format!(
                "Create a Corbanu Terminal pane using {}.",
                self.native_spawn_default_model()
            )),
            is_default: true,
            actions: vec![Box::new({
                let target = target.clone();
                let duration_arg = duration_arg.clone();
                let whip_name = whip_name.clone();
                move |tx| {
                    tx.send(AppEvent::CreateOrchestrateManager {
                        target: target.clone(),
                        duration_arg: duration_arg.clone(),
                        whip_name: whip_name.clone(),
                    });
                }
            })],
            dismiss_on_select: true,
            ..Default::default()
        });
        for entry in self.orchestrate_target_entries() {
            if worker_node_id.as_deref() == Some(entry.node_id.as_str()) {
                continue;
            }
            if self
                .primary_thread_id
                .is_some_and(|thread_id| entry.node_id == thread_node_id(thread_id))
            {
                continue;
            }
            let manager_node_id = entry.node_id.clone();
            items.push(SelectionItem {
                name: format!("Bind {}", entry.name),
                description: Some(entry.description.clone()),
                is_current: entry.is_current,
                actions: vec![Box::new({
                    let target = target.clone();
                    let duration_arg = duration_arg.clone();
                    let duration_label = duration_label.clone();
                    let whip_name = whip_name.clone();
                    move |tx| {
                        tx.send(AppEvent::OpenOrchestrateConfirm {
                            target: target.clone(),
                            duration_arg: duration_arg.clone(),
                            duration_label: duration_label.clone(),
                            whip_name: whip_name.clone(),
                            manager_node_id: manager_node_id.clone(),
                        });
                    }
                })],
                dismiss_on_select: true,
                search_value: Some(format!("{} {}", entry.name, entry.description)),
                ..Default::default()
            });
        }
        self.chat_widget.show_selection_view(SelectionViewParams {
            view_id: Some("orchestrate-manager"),
            title: Some("New Assignment - Manager".to_string()),
            subtitle: Some(format!(
                "Choose who will manage Worker {}.",
                self.orchestrate_target_label(&target)
            )),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Search panes".to_string()),
            ..Default::default()
        });
    }

    pub(crate) fn open_orchestrate_confirm(
        &mut self,
        target: String,
        duration_arg: String,
        duration_label: String,
        whip_name: String,
        manager_node_id: String,
    ) {
        let target_label = self.orchestrate_target_label(&target);
        let manager_label = self.node_label(&manager_node_id);
        let replacement =
            self.resolve_orchestrate_target_node(&target)
                .ok()
                .and_then(|target_node| {
                    self.orchestrate_whips
                        .values()
                        .find(|whip| {
                            whip.target == target_node && whip.state != WhipState::Detached
                        })
                        .map(|whip| whip.id.clone())
                });
        let attach_label = if let Some(id) = replacement.as_deref() {
            format!("Create assignment and replace {id}")
        } else {
            "Create assignment".to_string()
        };
        let mut items = vec![SelectionItem {
            name: attach_label,
            description: Some(format!(
                "Manager {manager_label} -> Worker {target_label}; {whip_name}; mandate cadence 15m"
            )),
            actions: vec![Box::new({
                let args = orchestrate_guided_attach_args(
                    &target,
                    &duration_arg,
                    &whip_name,
                    &manager_node_id,
                );
                move |tx| {
                    tx.send(AppEvent::HandleOrchestrateCommand { args: args.clone() });
                }
            })],
            dismiss_on_select: true,
            ..Default::default()
        }];
        items.push(SelectionItem {
            name: "Cancel".to_string(),
            description: Some("Leave assignments unchanged.".to_string()),
            actions: Vec::new(),
            dismiss_on_select: true,
            ..Default::default()
        });

        self.chat_widget.show_selection_view(SelectionViewParams {
            view_id: Some("orchestrate-confirm"),
            title: Some("New Assignment - Confirm".to_string()),
            subtitle: Some(format!(
                "Manager {manager_label} -> Worker {target_label}; {duration_label}; {whip_name}"
            )),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    pub(crate) fn open_orchestrate_write_whip_prompt(
        &mut self,
        target: String,
        duration_arg: String,
        duration_label: String,
    ) {
        let tx = self.app_event_tx.clone();
        let template = "Continue the assigned work.\n\nWhen the work is genuinely finished, report done and emit WHIP_DONE.".to_string();
        let view = CustomPromptView::new(
            "Write Assignment Spec".to_string(),
            "Standing instructions for the Manager".to_string(),
            template,
            Some(format!(
                "{} for {}",
                self.orchestrate_target_label(&target),
                duration_label
            )),
            Box::new(move |instructions: String| {
                tx.send(AppEvent::OpenOrchestrateSaveWhipPrompt {
                    target: target.clone(),
                    duration_arg: duration_arg.clone(),
                    duration_label: duration_label.clone(),
                    instructions,
                });
            }),
        )
        .with_submit_mode(CustomPromptSubmitMode::CtrlD);
        self.chat_widget.show_custom_prompt_view(view);
    }

    pub(crate) fn open_orchestrate_save_whip_prompt(
        &mut self,
        target: String,
        duration_arg: String,
        duration_label: String,
        instructions: String,
    ) {
        let tx = self.app_event_tx.clone();
        let suggested_name = suggested_whip_name(&instructions);
        let view = CustomPromptView::new(
            "Save Assignment Spec".to_string(),
            "Basename, for example keep-going".to_string(),
            suggested_name,
            Some("Saved under $CODEX_HOME/whips".to_string()),
            Box::new(move |requested_name: String| {
                tx.send(AppEvent::SaveOrchestrateWhipAndConfirm {
                    target: target.clone(),
                    duration_arg: duration_arg.clone(),
                    duration_label: duration_label.clone(),
                    requested_name,
                    instructions: instructions.clone(),
                });
            }),
        );
        self.chat_widget.show_custom_prompt_view(view);
    }

    pub(crate) fn save_orchestrate_whip_and_open_confirm(
        &mut self,
        target: String,
        duration_arg: String,
        duration_label: String,
        requested_name: String,
        instructions: String,
    ) {
        match save_global_whip_instruction(
            self.config.codex_home.as_ref(),
            &requested_name,
            &instructions,
        ) {
            Ok(whip_name) => {
                self.chat_widget.add_info_message(
                    format!("Saved assignment spec `{whip_name}`."),
                    Some("Saved globally under $CODEX_HOME/whips.".to_string()),
                );
                self.open_orchestrate_manager_picker(
                    target,
                    duration_arg,
                    duration_label,
                    whip_name,
                );
            }
            Err(err) => self.chat_widget.add_error_message(err),
        }
    }

    pub(crate) fn open_orchestrate_extend_duration_picker(&mut self, whip_id: String) {
        let choices = [
            ("15 minutes", "15m", "Add a short follow-up window."),
            ("1 hour", "1h", "Add one working window."),
            ("4 hours", "4h", "Add the default budget window."),
            ("8 hours", "8h", "Add an overnight window."),
        ];
        let items = choices
            .into_iter()
            .map(|(label, duration_arg, description)| {
                let args = format!("extend {whip_id} {duration_arg}");
                SelectionItem {
                    name: label.to_string(),
                    description: Some(description.to_string()),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::HandleOrchestrateCommand { args: args.clone() });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();

        let title = if self
            .orchestrate_whips
            .get(&whip_id)
            .is_some_and(Whip::is_assignment)
        {
            "Extend Assignment"
        } else {
            "Extend Legacy Automation"
        };
        self.chat_widget.show_selection_view(SelectionViewParams {
            view_id: Some("orchestrate-extend"),
            title: Some(title.to_string()),
            subtitle: Some(whip_id),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    pub(crate) fn open_orchestrate_whip_details(&mut self, whip_id: String) {
        let Some(whip) = self.orchestrate_whips.get(&whip_id).cloned() else {
            self.chat_widget
                .add_error_message(format!("No whip found for `{whip_id}`."));
            return;
        };
        let target = self.node_label(&whip.target);
        let holder = whip
            .holder
            .as_deref()
            .map(|holder| self.node_label(holder))
            .unwrap_or_else(|| "none".to_string());
        let expiry = assignment_expiry_label(&whip, self.orchestrate_now());
        let pause_resume = if whip.state == WhipState::Paused {
            ("Resume", format!("resume {whip_id}"))
        } else {
            ("Pause", format!("pause {whip_id}"))
        };
        let is_assignment = whip.is_assignment();
        let mut items = vec![
            SelectionItem {
                name: pause_resume.0.to_string(),
                description: Some(format!("{} assignment {}.", pause_resume.0, whip_id)),
                actions: vec![Box::new({
                    let args = pause_resume.1;
                    move |tx| tx.send(AppEvent::HandleOrchestrateCommand { args: args.clone() })
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Extend...".to_string(),
                description: Some("Add time to the current expiry.".to_string()),
                actions: vec![Box::new({
                    let whip_id = whip_id.clone();
                    move |tx| {
                        tx.send(AppEvent::OpenOrchestrateExtendDurationPicker {
                            whip_id: whip_id.clone(),
                        });
                    }
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
        ];
        if is_assignment
            && !matches!(
                whip.kind,
                WhipKind::Assignment {
                    phase: AssignmentPhase::Drafting,
                    ..
                }
            )
        {
            items.push(orchestrate_command_item(
                "Mandate now",
                "Ask the Manager to check progress immediately.",
                format!("fire {whip_id}"),
            ));
            items.push(orchestrate_command_item(
                "Test",
                "Preview the next Manager mandate.",
                format!("test {whip_id}"),
            ));
        } else {
            items.push(orchestrate_command_item(
                "Fire now",
                "Send the automation turn immediately.",
                format!("fire {whip_id}"),
            ));
            items.push(orchestrate_command_item(
                "Test",
                "Preview the next automation turn.",
                format!("test {whip_id}"),
            ));
        }
        items.push(orchestrate_command_item(
            "End",
            "End this assignment.",
            format!("detach {whip_id}"),
        ));
        items.push(SelectionItem {
            name: "New assignment".to_string(),
            description: Some("Open the guided setup flow.".to_string()),
            display_shortcut: Some(key_hint::plain(KeyCode::Char('n'))),
            actions: vec![Box::new(|tx| {
                tx.send(AppEvent::OpenOrchestrateTargetPicker)
            })],
            dismiss_on_select: true,
            ..Default::default()
        });

        let (title, subtitle) = if is_assignment {
            let phase = match &whip.kind {
                WhipKind::Assignment { phase, .. } => assignment_phase_label(phase),
                WhipKind::LegacyNudge => unreachable!(),
            };
            (
                format!("Assignment {whip_id}"),
                format!(
                    "Manager {holder} -> Worker {target}; {phase}; {expiry}; spec {}",
                    whip.instructions
                ),
            )
        } else {
            (
                format!("Legacy automation {whip_id}"),
                format!(
                    "{holder} -> {target}; {}; {}/{} runs; {expiry}; {}",
                    whip.mode.label(),
                    whip.fires,
                    whip.max_fires,
                    whip.instructions
                ),
            )
        };

        self.chat_widget.show_selection_view(SelectionViewParams {
            view_id: Some("orchestrate-details"),
            title: Some(title),
            subtitle: Some(subtitle),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    fn orchestrate_target_entries(&self) -> Vec<OrchestrateTargetEntry> {
        let mut entries = Vec::new();
        for (thread_id, entry) in self.agent_navigation.ordered_threads() {
            let node_id = thread_node_id(thread_id);
            let name = self.thread_label(thread_id);
            let status = if entry.is_closed {
                "done"
            } else if entry.is_running {
                "running"
            } else {
                "idle"
            };
            let mut description = format!("Corbanu Terminal; {status}; {thread_id}");
            if let Some(task) = entry.last_task_message.as_deref() {
                description.push_str(&format!(
                    "; task: {}",
                    truncate_for_orchestrate_display(task, /*max_chars*/ 90)
                ));
            }
            if let Some(result) = entry.last_result_message.as_deref() {
                description.push_str(&format!(
                    "; result: {}",
                    truncate_for_orchestrate_display(result, /*max_chars*/ 90)
                ));
            }
            entries.push(OrchestrateTargetEntry {
                target: thread_id.to_string(),
                node_id,
                name,
                description,
                is_current: self.claude_panes.active_user_pane_id() == CODEX_MAIN_PANE_ID
                    && self.active_thread_id == Some(thread_id),
            });
        }

        for pane in self.claude_panes.panes() {
            let status = match pane.status {
                ClaudePaneStatus::Idle => "idle",
                ClaudePaneStatus::Running => "running",
            };
            let mut description = format!(
                "Claude Code {}; {status}; {}",
                pane.profile.status_model_label(),
                pane.id
            );
            if let Some(turn_status) = pane.latest_turn_status {
                description.push_str(&format!("; latest: {}", turn_status.label()));
            }
            if let Some(usage) = pane.latest_usage_summary.as_deref() {
                description.push_str(&format!(
                    "; usage: {}",
                    truncate_for_orchestrate_display(usage, /*max_chars*/ 80)
                ));
            }
            if let Some(task) = pane.latest_task_message.as_deref() {
                description.push_str(&format!(
                    "; task: {}",
                    truncate_for_orchestrate_display(task, /*max_chars*/ 90)
                ));
            }
            if let Some(result) = pane.latest_result_message.as_deref() {
                description.push_str(&format!(
                    "; result: {}",
                    truncate_for_orchestrate_display(result, /*max_chars*/ 90)
                ));
            }
            entries.push(OrchestrateTargetEntry {
                target: pane.id.clone(),
                node_id: pane_node_id(&pane.id),
                name: pane.title.clone(),
                description,
                is_current: self.claude_panes.active_user_pane_id() == pane.id,
            });
        }

        entries.sort_by(|left, right| left.name.cmp(&right.name));
        entries
    }

    fn orchestrate_target_label(&self, target: &str) -> String {
        self.resolve_orchestrate_target_node(target)
            .ok()
            .and_then(|node_id| self.spawn_node_title(&node_id))
            .unwrap_or_else(|| target.to_string())
    }

    pub(crate) fn dispatch_orchestrate_blocks_from_text(
        &mut self,
        source_node_id: &str,
        text: &str,
    ) -> bool {
        let (_visible, blocks) = extract_orchestrate_blocks(text);
        if blocks.is_empty() {
            return false;
        }
        for block in blocks {
            self.apply_orchestrate_command(block.command, CommandOrigin::Agent(source_node_id));
        }
        true
    }

    pub(crate) fn whip_status_suffix_for_target(&self, target_node_id: &str) -> String {
        if let Some(assignment) = self.orchestrate_whips.values().find(|whip| {
            whip.is_assignment()
                && whip.state == WhipState::Armed
                && (whip.target == target_node_id || whip.holder.as_deref() == Some(target_node_id))
        }) {
            let phase = match &assignment.kind {
                WhipKind::Assignment { phase, .. } => assignment_phase_label(phase),
                WhipKind::LegacyNudge => unreachable!(),
            };
            if assignment.target == target_node_id {
                let manager = assignment
                    .holder
                    .as_deref()
                    .map(|node| self.node_label(node))
                    .unwrap_or_else(|| "missing Manager".to_string());
                return format!("; managed-by {manager}; {phase}");
            }
            return format!(
                "; managing {}; {phase}",
                self.node_label(&assignment.target)
            );
        }
        whip_suffix_for_target(&self.orchestrate_whips, target_node_id)
    }

    pub(crate) fn note_whip_target_started(&mut self, target_node_id: &str) {
        self.orchestrate_idle_generation_by_target
            .entry(target_node_id.to_string())
            .or_insert(0);
    }

    pub(crate) fn note_whip_target_idle_with_fire_control(
        &mut self,
        target_node_id: &str,
        last_output: Option<&str>,
        allow_fire: bool,
        turn_succeeded: bool,
    ) {
        let generation = self
            .orchestrate_idle_generation_by_target
            .entry(target_node_id.to_string())
            .or_insert(0);
        *generation = generation.saturating_add(1);
        let generation = *generation;
        let is_assignment_worker = self.orchestrate_whips.values().any(|whip| {
            whip.is_assignment()
                && whip.state == WhipState::Armed
                && whip.target == target_node_id
                && matches!(
                    whip.kind,
                    WhipKind::Assignment {
                        phase: AssignmentPhase::Executing,
                        ..
                    }
                )
        });
        let completed_output = last_output
            .filter(|output| !output.trim().is_empty())
            .map(str::to_string)
            .or_else(|| {
                is_assignment_worker.then(|| {
                    if turn_succeeded {
                        "Worker turn completed successfully without visible output.".to_string()
                    } else {
                        "Worker turn ended unsuccessfully without visible output.".to_string()
                    }
                })
            });
        if let Some(output) = completed_output {
            let output = output.chars().take(12_000).collect::<String>();
            let mut changed = false;
            for whip in self.orchestrate_whips.values_mut().filter(|whip| {
                whip.is_assignment()
                    && whip.state == WhipState::Armed
                    && whip.target == target_node_id
            }) {
                whip.last_target_output = Some(output.clone());
                changed = true;
            }
            if changed {
                self.persist_pane_state();
            }
        }
        self.pause_matching_whips_on_stop_marker(target_node_id, last_output);
        self.recover_assignment_manager_on_empty_output(
            target_node_id,
            last_output,
            turn_succeeded,
        );
        self.pause_spinning_whips_on_empty_output(target_node_id, last_output);
        self.pause_spinning_whips_on_failed_turn(target_node_id, turn_succeeded);
        self.note_whip_holder_idle(target_node_id);
        if allow_fire {
            let trigger = if is_assignment_worker {
                FireTrigger::Completion
            } else {
                FireTrigger::Edge
            };
            self.evaluate_whips_for_target(target_node_id, generation, trigger);
        }
    }

    pub(crate) fn note_whip_holder_dispatched(
        &mut self,
        holder_node_id: &str,
        target_node_id: &str,
    ) {
        let now = self.orchestrate_now();
        let holder_node_id = normalize_orchestrate_node_id(holder_node_id);
        let mut changed = false;
        for whip in self.orchestrate_whips.values_mut().filter(|whip| {
            whip.is_assignment()
                && whip.holder.as_deref() == Some(holder_node_id.as_str())
                && whip.target == target_node_id
        }) {
            if let WhipKind::Assignment {
                phase,
                execution_started_utc,
                last_user_turn_utc,
                execution_duration_s,
                ..
            } = &mut whip.kind
                && matches!(
                    phase,
                    AssignmentPhase::Drafting | AssignmentPhase::Blocked { .. }
                )
            {
                let was_drafting = matches!(phase, AssignmentPhase::Drafting);
                *phase = AssignmentPhase::Executing;
                changed = true;
                if was_drafting {
                    // Creating the assignment is user activity, but once its Manager has accepted
                    // the spec and dispatched the first Worker task it must not suppress the first
                    // completion handoff. Later operator turns set this timestamp again.
                    *last_user_turn_utc = None;
                }
                if execution_started_utc.is_none() {
                    *execution_started_utc = Some(now);
                    whip.expires_at =
                        execution_duration_s.map(|seconds| now + Duration::seconds(seconds));
                }
            }
        }
        for whip in self.orchestrate_whips.values_mut().filter(|whip| {
            !whip.is_assignment()
                && whip.mode == WhipMode::Review
                && whip.holder.as_deref() == Some(holder_node_id.as_str())
                && whip.target == target_node_id
        }) {
            whip.pending_review_fire = None;
            whip.ignored_review_fires = 0;
            changed = true;
        }
        if changed {
            self.persist_pane_state();
        }
    }

    pub(crate) fn note_native_collab_assignment_dispatch(
        &mut self,
        sender_thread_id: &str,
        receiver_thread_ids: &[String],
    ) {
        let Ok(sender_thread_id) = ThreadId::from_string(sender_thread_id) else {
            return;
        };
        let holder_node_id = self.spawn_orchestration_node_for_thread(sender_thread_id);
        for receiver_thread_id in receiver_thread_ids {
            let Ok(receiver_thread_id) = ThreadId::from_string(receiver_thread_id) else {
                continue;
            };
            let target_node_id = self.spawn_orchestration_node_for_thread(receiver_thread_id);
            let matches_active_assignment = self.orchestrate_whips.values().any(|whip| {
                whip.is_assignment()
                    && whip.state == WhipState::Armed
                    && whip.holder.as_deref() == Some(holder_node_id.as_str())
                    && whip.target == target_node_id
                    && !matches!(
                        whip.kind,
                        WhipKind::Assignment {
                            phase: AssignmentPhase::Done,
                            ..
                        }
                    )
            });
            if !matches_active_assignment {
                continue;
            }
            self.note_whip_holder_dispatched(&holder_node_id, &target_node_id);
            self.note_assignment_dispatch_delivered(&holder_node_id, &target_node_id);
        }
    }

    pub(crate) fn is_assignment_holder(&self, node_id: &str) -> bool {
        let node_id = normalize_orchestrate_node_id(node_id);
        self.orchestrate_whips.values().any(|whip| {
            whip.is_assignment()
                && whip.state == WhipState::Armed
                && whip.holder.as_deref() == Some(node_id.as_str())
        })
    }

    pub(crate) fn is_orchestration_participant(&self, node_id: &str) -> bool {
        let node_id = normalize_orchestrate_node_id(node_id);
        self.orchestrate_whips
            .values()
            .any(|whip| whip.target == node_id || whip.holder.as_deref() == Some(node_id.as_str()))
    }

    pub(crate) fn assignment_dispatch_target_for_holder(
        &self,
        holder_node_id: &str,
    ) -> Option<(String, String)> {
        let holder_node_id = normalize_orchestrate_node_id(holder_node_id);
        self.orchestrate_whips.values().find_map(|whip| {
            (whip.is_assignment()
                && whip.state == WhipState::Armed
                && whip.holder.as_deref() == Some(holder_node_id.as_str())
                && !matches!(
                    whip.kind,
                    WhipKind::Assignment {
                        phase: AssignmentPhase::Done,
                        ..
                    }
                ))
            .then(|| (whip.id.clone(), whip.target.clone()))
        })
    }

    pub(crate) fn note_assignment_dispatch_failure(
        &mut self,
        holder_node_id: &str,
        cause: &str,
        will_retry: bool,
    ) {
        self.note_assignment_dispatch_failure_with_turns(
            holder_node_id,
            cause,
            will_retry,
            /*start_turns*/ true,
        );
    }

    /// `start_turns` false (#660): the Manager is told without starting a
    /// turn in it (a dispatch sent while the kill switch was on, or held or
    /// refused because of it).
    pub(crate) fn note_assignment_dispatch_failure_with_turns(
        &mut self,
        holder_node_id: &str,
        cause: &str,
        will_retry: bool,
        start_turns: bool,
    ) {
        let holder_node_id = normalize_orchestrate_node_id(holder_node_id);
        let Some((id, worker_node_id)) =
            self.assignment_dispatch_target_for_holder(&holder_node_id)
        else {
            return;
        };
        let cause = assignment_user_facing_dispatch_cause(cause);
        let manager_label = self.node_label(&holder_node_id);
        let worker_label = self.node_label(&worker_node_id);
        if let Some(whip) = self.orchestrate_whips.get_mut(&id) {
            whip.consecutive_failed_turns = whip.consecutive_failed_turns.saturating_add(1);
            if let WhipKind::Assignment {
                failure_backoff_level,
                ..
            } = &mut whip.kind
            {
                *failure_backoff_level = failure_backoff_level.saturating_add(1).min(3);
            }
            whip.last_dispatch_result = Some(if will_retry {
                format!("failed ({cause}); retrying durable Worker ID")
            } else {
                format!("retry failed ({cause}); assignment paused")
            });
            if !will_retry {
                whip.state = WhipState::Paused;
            }
        }
        let message = if will_retry {
            format!(
                "Assignment {id} dispatch failed: Manager {manager_label} -> Worker {worker_label}: {cause}. Retrying once using durable Worker ID `{worker_node_id}`."
            )
        } else {
            format!(
                "Assignment {id} dispatch retry failed: Manager {manager_label} -> Worker {worker_label}: {cause}. Assignment paused."
            )
        };
        self.chat_widget.add_error_message(message.clone());
        // The active UI can change between an automated dispatch and its failure. Persist the
        // operational notice to the Manager as a report as well, so switching panes cannot hide
        // the retry or pause reason from the operator or the Manager's next turn.
        self.record_spawn_parent_report_with_notify(
            holder_node_id,
            format!("assignment_dispatch_notice; {message}"),
            /*notify*/ true,
            start_turns,
        );
        self.persist_pane_state();
    }

    pub(crate) fn note_assignment_dispatch_delivered(
        &mut self,
        holder_node_id: &str,
        target_node_id: &str,
    ) {
        let holder_node_id = normalize_orchestrate_node_id(holder_node_id);
        let target_node_id = normalize_orchestrate_node_id(target_node_id);
        let Some(id) = self.orchestrate_whips.values().find_map(|whip| {
            (whip.is_assignment()
                && whip.state == WhipState::Armed
                && whip.holder.as_deref() == Some(holder_node_id.as_str())
                && whip.target == target_node_id
                && !matches!(
                    whip.kind,
                    WhipKind::Assignment {
                        phase: AssignmentPhase::Done,
                        ..
                    }
                ))
            .then(|| whip.id.clone())
        }) else {
            return;
        };
        if let Some(whip) = self.orchestrate_whips.get_mut(&id) {
            whip.last_dispatch_result = Some("delivered".to_string());
            whip.consecutive_failed_turns = 0;
            if let WhipKind::Assignment {
                failure_backoff_level,
                ..
            } = &mut whip.kind
            {
                *failure_backoff_level = 0;
            }
        }
        self.persist_pane_state();
    }

    pub(crate) fn note_assignment_user_turn(&mut self, node_id: &str) {
        let node_id = normalize_orchestrate_node_id(node_id);
        let now = self.orchestrate_now();
        let mut changed = false;
        for whip in self.orchestrate_whips.values_mut().filter(|whip| {
            whip.is_assignment()
                && (whip.holder.as_deref() == Some(node_id.as_str()) || whip.target == node_id)
        }) {
            if let WhipKind::Assignment {
                phase,
                last_user_turn_utc,
                ..
            } = &mut whip.kind
            {
                *last_user_turn_utc = Some(now);
                changed = true;
                if whip.holder.as_deref() == Some(node_id.as_str())
                    && matches!(phase, AssignmentPhase::Blocked { .. })
                {
                    *phase = AssignmentPhase::Executing;
                }
            }
        }
        if changed {
            self.persist_pane_state();
        }
    }

    pub(crate) fn note_assignment_node_gone(&mut self, node_id: &str) {
        let node_id = normalize_orchestrate_node_id(node_id);
        let node_label = self.node_label(&node_id);
        let mut notices = Vec::new();
        for whip in self.orchestrate_whips.values_mut().filter(|whip| {
            whip.is_assignment()
                && whip.state == WhipState::Armed
                && !matches!(
                    whip.kind,
                    WhipKind::Assignment {
                        phase: AssignmentPhase::Done,
                        ..
                    }
                )
                && (whip.target == node_id || whip.holder.as_deref() == Some(node_id.as_str()))
        }) {
            let role = if whip.target == node_id {
                "Worker"
            } else {
                "Manager"
            };
            whip.state = WhipState::Paused;
            notices.push((whip.id.clone(), role));
        }
        let changed = !notices.is_empty();
        for (id, role) in notices {
            self.chat_widget.add_info_message(
                format!("Assignment {id} paused: {role} {node_label} is unavailable."),
                /*hint*/ None,
            );
        }
        if changed {
            self.persist_pane_state();
        }
    }

    pub(crate) fn audit_restored_assignments(&mut self) {
        let assignments: Vec<(String, String, Option<String>, bool)> = self
            .orchestrate_whips
            .values()
            .filter_map(|whip| {
                if whip.state != WhipState::Armed {
                    return None;
                }
                let WhipKind::Assignment { phase, .. } = &whip.kind else {
                    return None;
                };
                if matches!(phase, AssignmentPhase::Done) {
                    return None;
                }
                Some((
                    whip.id.clone(),
                    whip.target.clone(),
                    whip.holder.clone(),
                    matches!(phase, AssignmentPhase::Executing),
                ))
            })
            .collect();
        let has_assignments = !assignments.is_empty();
        for (id, worker, manager, executing) in assignments {
            let missing_role = if let Err(err) = self.fire_destination_for_node(&worker) {
                Some(("Worker", err))
            } else if let Some(err) = manager
                .as_deref()
                .and_then(|node| self.fire_destination_for_node(node).err())
            {
                Some(("Manager", err))
            } else if manager.is_none() {
                Some(("Manager", "No Manager is bound.".to_string()))
            } else {
                None
            };
            if let Some((role, detail)) = missing_role {
                if let Some(whip) = self.orchestrate_whips.get_mut(&id) {
                    whip.state = WhipState::Paused;
                }
                self.chat_widget.add_info_message(
                    format!(
                        "Assignment {id} paused after restart: {role} is unavailable ({detail})."
                    ),
                    /*hint*/ None,
                );
            } else if executing {
                self.chat_widget.add_info_message(
                    format!(
                        "Assignment {id} restored; the next Manager mandate waits one cadence."
                    ),
                    /*hint*/ None,
                );
            }
        }
        if has_assignments {
            self.persist_pane_state();
        }
    }

    pub(crate) fn sweep_orchestrate_whips(&mut self) {
        if std::env::var("PFTERMINAL_ORCHESTRATE_QA").as_deref() == Ok("1")
            && let Some(thread_id) = std::env::var("PFTERMINAL_ORCHESTRATE_QA_CONTROL")
                .ok()
                .and_then(|path| fs::read_to_string(path).ok())
                .and_then(|control| {
                    control
                        .lines()
                        .find_map(|line| line.trim().strip_prefix("close="))
                        .and_then(node_id_thread)
                })
        {
            self.mark_agent_picker_thread_closed(thread_id);
        }
        let now = self.orchestrate_now();
        let ids: Vec<String> = self.orchestrate_whips.keys().cloned().collect();
        for id in ids {
            self.expire_whip_if_needed(&id, now);
        }
        if self.pause_whips_for_kill_switch() {
            return;
        }
        self.watch_assignment_reachability(now);
        let target_generations: Vec<(String, u64)> = self
            .orchestrate_whips
            .values()
            .filter(|whip| whip.state == WhipState::Armed)
            .map(|whip| {
                let generation = self
                    .orchestrate_idle_generation_by_target
                    .get(&whip.target)
                    .copied()
                    .unwrap_or(0);
                (whip.target.clone(), generation)
            })
            .collect();
        for (target, generation) in target_generations {
            self.evaluate_whips_for_target(&target, generation, FireTrigger::Tick);
        }
    }

    fn watch_assignment_reachability(&mut self, now: DateTime<Utc>) {
        let ids: Vec<String> = self
            .orchestrate_whips
            .values()
            .filter(|whip| {
                whip.state == WhipState::Armed
                    && matches!(
                        whip.kind,
                        WhipKind::Assignment {
                            phase: AssignmentPhase::Executing,
                            ..
                        }
                    )
            })
            .map(|whip| whip.id.clone())
            .collect();
        let mut pause = Vec::new();
        let mut changed = false;
        for id in ids {
            let Some(snapshot) = self.orchestrate_whips.get(&id).cloned() else {
                continue;
            };
            let manager = snapshot.holder.as_deref();
            let worker_unreachable = self.fire_destination_for_node(&snapshot.target).is_err()
                || !self.target_node_is_idle(&snapshot.target);
            let manager_unreachable = manager.is_none_or(|node| {
                self.fire_destination_for_node(node).is_err() || !self.target_node_is_idle(node)
            });
            let unreachable = worker_unreachable || manager_unreachable;
            let since = if let Some(whip) = self.orchestrate_whips.get_mut(&id) {
                if unreachable {
                    if whip.assignment_unreachable_since_utc.is_none() {
                        whip.assignment_unreachable_since_utc = Some(now);
                        changed = true;
                    }
                    let since = whip.assignment_unreachable_since_utc.unwrap_or(now);
                    Some(since)
                } else {
                    changed |= whip.assignment_unreachable_since_utc.take().is_some();
                    None
                }
            } else {
                None
            };
            if since.is_some_and(|since| {
                now - since >= Duration::seconds((snapshot.cooldown_s.saturating_mul(4)) as i64)
            }) {
                let role = if worker_unreachable {
                    "Worker"
                } else {
                    "Manager"
                };
                pause.push((id, role));
            }
        }
        for (id, role) in pause {
            if let Some(whip) = self.orchestrate_whips.get_mut(&id) {
                whip.state = WhipState::Paused;
            }
            self.chat_widget.add_info_message(
                format!("Assignment {id} paused: {role} unreachable."),
                /*hint*/ None,
            );
            changed = true;
        }
        if changed {
            self.persist_pane_state();
        }
    }

    fn apply_orchestrate_command(
        &mut self,
        command: OrchestrateCommand,
        origin: CommandOrigin<'_>,
    ) {
        if let Err(err) = self.authorize_orchestrate_command(&command, origin) {
            self.chat_widget.add_error_message(err);
            return;
        }
        // #613: while the kill switch is on nothing is resumed or started
        // (it could fire right after switch-off), and a whip attached now is
        // paused at once, like those armed when the switch went on. The saved
        // state decides (not the state this process last learned): a switch-off
        // elsewhere allows it at once, and a switch-on is already saved.
        let refused = match &command {
            OrchestrateCommand::Resume(id) => Some(("resumed", "resume", id)),
            OrchestrateCommand::Start(id) => Some(("started", "start", id)),
            _ => None,
        };
        if let Some((done, verb, id_or_target)) = refused
            && crate::legacy_core::security_revocation::kill_switch_on(
                self.config.codex_home.as_ref(),
            )
        {
            self.chat_widget.add_error_message(format!(
                "Not {done}: the security kill switch is on. Turn it off in /security, then run /orchestrate {verb} {id_or_target}."
            ));
            return;
        }
        let attaches = matches!(command, OrchestrateCommand::Attach { .. });
        self.dispatch_orchestrate_command(command, origin);
        if attaches {
            self.pause_whips_for_kill_switch();
        }
    }

    fn dispatch_orchestrate_command(
        &mut self,
        command: OrchestrateCommand,
        origin: CommandOrigin<'_>,
    ) {
        match command {
            OrchestrateCommand::Status => self.chat_widget.add_info_message(
                format_whip_status(&self.orchestrate_whips, self.orchestrate_now()),
                /*hint*/ None,
            ),
            OrchestrateCommand::Attach {
                target,
                whip_name,
                mode,
                expiry,
                max_fires,
                cooldown_s,
                holder,
            } => {
                match self.attach_whip(
                    target, whip_name, mode, expiry, max_fires, cooldown_s, holder, origin,
                ) {
                    Ok(message) => self.chat_widget.add_info_message(message, /*hint*/ None),
                    Err(err) => self.chat_widget.add_error_message(err),
                }
            }
            OrchestrateCommand::Detach(id_or_target) => {
                self.set_whip_state_by_ref(&id_or_target, WhipState::Detached, "detached")
            }
            OrchestrateCommand::Pause(id_or_target) => {
                self.set_whip_state_by_ref(&id_or_target, WhipState::Paused, "paused")
            }
            OrchestrateCommand::Resume(id_or_target) => {
                self.set_whip_state_by_ref(&id_or_target, WhipState::Armed, "resumed")
            }
            OrchestrateCommand::Start(id_or_target) => self.start_assignment_by_ref(&id_or_target),
            OrchestrateCommand::Extend { id, duration } => {
                let now = self.orchestrate_now();
                let Some(whip) = self.orchestrate_whips.get_mut(&id) else {
                    self.chat_widget
                        .add_error_message(format!("No whip found for `{id}`."));
                    return;
                };
                let base = whip
                    .expires_at
                    .filter(|expiry| *expiry > now)
                    .unwrap_or(now);
                whip.expires_at = Some(base + Duration::seconds(duration.seconds));
                whip.expiry_notified = false;
                let expires = whip
                    .expires_at
                    .map(|value| value.to_rfc3339())
                    .unwrap_or_else(|| "unlimited".to_string());
                self.persist_pane_state();
                self.chat_widget.add_info_message(
                    format!("Extended {id}; expires_at={expires}."),
                    /*hint*/ None,
                );
            }
            OrchestrateCommand::Fire(id) => match self.plan_whip_fire(&id, FireTrigger::Manual) {
                Ok(plan) => self.execute_whip_fire(plan, FireTrigger::Manual),
                Err(err) => self.chat_widget.add_error_message(err),
            },
            OrchestrateCommand::Test(id) => match self.plan_whip_fire(&id, FireTrigger::Test) {
                Ok(plan) => self.chat_widget.add_info_message(
                    format!(
                        "Whip {} would send a {} turn to {}:\n{}",
                        plan.whip_id,
                        if self
                            .orchestrate_whips
                            .get(&plan.whip_id)
                            .is_some_and(|whip| whip.mode == WhipMode::Review)
                        {
                            "review"
                        } else {
                            "task"
                        },
                        plan.destination_label,
                        plan.task
                    ),
                    /*hint*/ None,
                ),
                Err(err) => self.chat_widget.add_error_message(err),
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn attach_whip(
        &mut self,
        target: String,
        whip_name: String,
        mode: Option<WhipMode>,
        expiry: Option<ExpiryArg>,
        max_fires: Option<u32>,
        cooldown_s: Option<u64>,
        holder: Option<HolderArg>,
        origin: CommandOrigin<'_>,
    ) -> Result<String, String> {
        let (instruction_path, instruction_text) = if whip_name == DRAFT_WITH_MANAGER_SPEC {
            (PathBuf::from("Draft with Manager"), String::new())
        } else {
            read_whip_instruction(
                self.config.codex_home.as_ref(),
                &self.config.cwd,
                &whip_name,
            )?
        };
        let doc_defaults = parse_whip_doc_defaults(&instruction_text);
        let target_node_id = self.resolve_orchestrate_target_node(&target)?;
        let mut resolved_mode = mode.or(doc_defaults.mode).unwrap_or(WhipMode::Review);
        let holder_arg = holder.unwrap_or_else(|| match origin {
            CommandOrigin::Agent(node) => HolderArg::Target(node.to_string()),
            CommandOrigin::User => HolderArg::Me,
        });
        let holder_node_id = match holder_arg {
            HolderArg::None => {
                resolved_mode = WhipMode::Auto;
                None
            }
            HolderArg::Me => Some(match origin {
                CommandOrigin::User => self.current_holder_node()?,
                CommandOrigin::Agent(node) => normalize_orchestrate_node_id(node),
            }),
            HolderArg::Target(value) => Some(self.resolve_orchestrate_target_node(&value)?),
        };
        if resolved_mode == WhipMode::Review
            && self.primary_thread_id.is_some_and(|thread_id| {
                holder_node_id.as_deref() == Some(thread_node_id(thread_id).as_str())
            })
        {
            return Err(
                "Corbanu Terminal Main cannot be an assignment Manager; create a Manager pane."
                    .to_string(),
            );
        }
        if resolved_mode == WhipMode::Review && holder_node_id.is_none() {
            return Err(
                "Review-mode whips require a holder; use --holder me or --mode auto.".to_string(),
            );
        }
        if holder_node_id.as_deref() == Some(target_node_id.as_str()) {
            return Err("A whip holder cannot be the same pane as its target.".to_string());
        }
        if let Some(manager) = holder_node_id.as_deref() {
            self.fire_destination_for_node(manager)?;
        }
        // One clock read for the whole attach: the requested duration is measured
        // from the same instant the assignment's duration is computed against.
        let now = self.orchestrate_now();
        let resolved_expiry = expiry
            .map(|value| resolve_expiry_arg(value, now))
            .transpose()?;
        let options = ResolvedAttachOptions {
            mode: Some(resolved_mode),
            expiry: resolved_expiry,
            max_fires: max_fires.or(doc_defaults.max_fires),
            cooldown_s: cooldown_s.or(doc_defaults.cooldown_s),
            stop_marker: doc_defaults.stop_marker,
        };
        let replaced: Vec<Whip> = self
            .orchestrate_whips
            .values()
            .filter(|whip| whip.target == target_node_id && whip.state != WhipState::Detached)
            .cloned()
            .collect();
        for whip in &replaced {
            self.orchestrate_whips.remove(&whip.id);
        }
        let requested_cooldown_s = options.cooldown_s;
        let create_assignment = resolved_mode == WhipMode::Review;
        let id = self.next_orchestrate_id(create_assignment);
        let mut whip = Whip::new(
            id.clone(),
            holder_node_id,
            target_node_id.clone(),
            whip_name.clone(),
            options,
            now,
        );
        if create_assignment {
            let execution_duration_s = whip
                .expires_at
                .map(|expiry| (expiry - now).num_seconds().max(1));
            whip.expires_at = None;
            whip.cooldown_s = requested_cooldown_s.unwrap_or_else(assignment_default_cadence_s);
            whip.kind = WhipKind::Assignment {
                phase: AssignmentPhase::Drafting,
                execution_started_utc: None,
                last_user_turn_utc: Some(now),
                failure_backoff_level: 0,
                execution_duration_s,
            };
        }
        self.orchestrate_whips.insert(id.clone(), whip);
        self.orchestrate_idle_generation_by_target
            .entry(target_node_id)
            .or_insert(0);
        if !self.persist_pane_state() {
            self.orchestrate_whips.remove(&id);
            for whip in replaced {
                self.orchestrate_whips.insert(whip.id.clone(), whip);
            }
            self.persist_pane_state();
            return Err(
                "Assignment was not created because its state could not be saved.".to_string(),
            );
        }
        if create_assignment {
            if let Err(err) = self.inject_assignment_birth_brief(&id) {
                self.orchestrate_whips.remove(&id);
                for whip in replaced {
                    self.orchestrate_whips.insert(whip.id.clone(), whip);
                }
                self.persist_pane_state();
                return Err(format!(
                    "Assignment was not created because its brief could not be delivered: {err}"
                ));
            }
            return Ok(format!(
                "Assignment {id} is ready in Drafting with spec `{whip_name}`."
            ));
        }
        Ok(format!(
            "Attached {id} to `{target}` with `{}` ({}) from {}.",
            whip_name,
            resolved_mode.label(),
            instruction_path.display()
        ))
    }

    /// #507: while the security kill switch is on, no assignment or whip
    /// fires on its own. Each armed one is paused, so none starts work when
    /// the switch is turned off either: a person resumes it. Returns whether
    /// the switch is on.
    pub(crate) fn pause_whips_for_kill_switch(&mut self) -> bool {
        if !self
            .orchestrate_whips
            .values()
            .any(|whip| whip.state == WhipState::Armed)
        {
            return false;
        }
        if !crate::legacy_core::security_revocation::kill_switch_on(self.config.codex_home.as_ref())
        {
            return false;
        }
        let mut paused: Vec<(String, bool)> = self
            .orchestrate_whips
            .values_mut()
            .filter(|whip| whip.state == WhipState::Armed)
            .map(|whip| {
                whip.state = WhipState::Paused;
                (whip.id.clone(), whip.is_assignment())
            })
            .collect();
        paused.sort();
        self.persist_pane_state();
        for (id, is_assignment) in paused {
            let subject = if is_assignment { "Assignment" } else { "Whip" };
            self.chat_widget.add_info_message(
                format!(
                    "{subject} {id} paused: the security kill switch is on. Turning it off does not resume it; resume it with /orchestrate resume {id} when you want it to run again."
                ),
                /*hint*/ None,
            );
        }
        true
    }

    fn set_whip_state_by_ref(&mut self, id_or_target: &str, state: WhipState, action: &str) {
        let Some(id) = self.find_whip_id(id_or_target) else {
            self.chat_widget
                .add_error_message(format!("No whip found for `{id_or_target}`."));
            return;
        };
        let is_assignment = self
            .orchestrate_whips
            .get(&id)
            .is_some_and(Whip::is_assignment);
        let removed_target = if state == WhipState::Detached {
            self.orchestrate_whips.remove(&id).map(|whip| whip.target)
        } else {
            None
        };
        if let Some(whip) = self.orchestrate_whips.get_mut(&id) {
            whip.state = state;
            if state == WhipState::Armed {
                whip.expiry_notified = false;
            }
        }
        if let Some(target) = removed_target
            && !self
                .orchestrate_whips
                .values()
                .any(|whip| whip.target == target)
        {
            self.orchestrate_idle_generation_by_target.remove(&target);
        }
        self.persist_pane_state();
        let subject = if is_assignment { "Assignment" } else { "Whip" };
        let action = if is_assignment && state == WhipState::Detached {
            "ended"
        } else {
            action
        };
        self.chat_widget
            .add_info_message(format!("{subject} {id} {action}."), /*hint*/ None);
    }

    fn start_assignment_by_ref(&mut self, id_or_target: &str) {
        let Some(id) = self.find_whip_id(id_or_target) else {
            self.chat_widget
                .add_error_message(format!("No assignment found for `{id_or_target}`."));
            return;
        };
        let now = self.orchestrate_now();
        let Some(whip) = self.orchestrate_whips.get_mut(&id) else {
            return;
        };
        let WhipKind::Assignment {
            phase,
            execution_started_utc,
            execution_duration_s,
            ..
        } = &mut whip.kind
        else {
            self.chat_widget
                .add_error_message(format!("{id} is a legacy nudge, not an assignment."));
            return;
        };
        *phase = AssignmentPhase::Executing;
        if execution_started_utc.is_none() {
            *execution_started_utc = Some(now);
            whip.expires_at = execution_duration_s.map(|seconds| now + Duration::seconds(seconds));
        }
        whip.state = WhipState::Armed;
        self.persist_pane_state();
        self.chat_widget.add_info_message(
            format!("Assignment {id} execution started."),
            /*hint*/ None,
        );
    }

    fn evaluate_whips_for_target(
        &mut self,
        target_node_id: &str,
        idle_generation: u64,
        trigger: FireTrigger,
    ) {
        if !matches!(trigger, FireTrigger::Manual | FireTrigger::Test)
            && self.pause_whips_for_kill_switch()
        {
            return;
        }
        let ids: Vec<String> = self
            .orchestrate_whips
            .values()
            .filter(|whip| whip.target == target_node_id)
            .map(|whip| whip.id.clone())
            .collect();
        for id in ids {
            match self.plan_whip_fire_for_generation(&id, idle_generation, trigger) {
                Ok(plan) => self.execute_whip_fire(plan, trigger),
                Err(err) if matches!(trigger, FireTrigger::Manual | FireTrigger::Test) => {
                    self.chat_widget.add_error_message(err);
                }
                Err(_) => {}
            }
        }
    }

    fn inject_assignment_birth_brief(&mut self, id: &str) -> Result<(), String> {
        let whip = self
            .orchestrate_whips
            .get(id)
            .cloned()
            .ok_or_else(|| format!("No assignment found for `{id}`."))?;
        let manager = whip
            .holder
            .as_deref()
            .ok_or_else(|| format!("Assignment {id} has no Manager."))?;
        let destination = self.fire_destination_for_node(manager)?;
        let worker_label = self.node_label(&whip.target);
        let manager_label = self.node_label(manager);
        let instruction = if whip.instructions == DRAFT_WITH_MANAGER_SPEC {
            Err("drafted with Manager".to_string())
        } else {
            read_whip_instruction(
                self.config.codex_home.as_ref(),
                &self.config.cwd,
                &whip.instructions,
            )
        };
        let task = assignment_birth_brief(
            &whip,
            &manager_label,
            &worker_label,
            instruction.as_ref().ok().map(|(_, text)| text.as_str()),
            instruction.as_ref().ok().map(|(path, _)| path.as_path()),
            self.assignment_dispatch_protocol_for_target(&whip.target),
        );
        match destination {
            FireDestination::Native(thread_id) => self.send_native_task_event(thread_id, task),
            FireDestination::ClaudePane(pane_id) => {
                let sent_while_kill_switch_on = self.kill_switch_on_now();
                self.app_event_tx.send(AppEvent::SubmitSpawnClaudePaneTask {
                    pane_id,
                    task,
                    sent_while_kill_switch_on,
                });
            }
        }
        self.chat_widget.add_info_message(
            format!("Assignment {id} started in Drafting: Manager {manager_label} -> Worker {worker_label}."),
            /*hint*/ None,
        );
        Ok(())
    }

    pub(crate) fn assignment_dispatch_protocol_for_target(
        &self,
        target: &str,
    ) -> AssignmentDispatchProtocol {
        match node_id_thread(target) {
            Some(thread_id) if self.is_native_spawn_thread(thread_id) => {
                AssignmentDispatchProtocol::NativeFollowup
            }
            _ => AssignmentDispatchProtocol::HostAdapter,
        }
    }

    fn plan_whip_fire(
        &mut self,
        id_or_target: &str,
        trigger: FireTrigger,
    ) -> Result<FirePlan, String> {
        let id = self
            .find_whip_id(id_or_target)
            .ok_or_else(|| format!("No whip found for `{id_or_target}`."))?;
        let target = self
            .orchestrate_whips
            .get(&id)
            .map(|whip| whip.target.clone())
            .ok_or_else(|| format!("No whip found for `{id}`."))?;
        let generation = self
            .orchestrate_idle_generation_by_target
            .get(&target)
            .copied()
            .unwrap_or(0);
        self.plan_whip_fire_for_generation(&id, generation, trigger)
    }

    fn plan_whip_fire_for_generation(
        &mut self,
        id: &str,
        idle_generation: u64,
        trigger: FireTrigger,
    ) -> Result<FirePlan, String> {
        let now = self.orchestrate_now();
        self.expire_whip_if_needed(id, now);
        let whip = self
            .orchestrate_whips
            .get(id)
            .ok_or_else(|| format!("No whip found for `{id}`."))?
            .clone();
        if !whip.is_armed() {
            return Err(format!("Whip {id} is {}.", whip.state.label()));
        }
        let is_assignment = whip.is_assignment();
        if is_assignment
            && !matches!(
                whip.kind,
                WhipKind::Assignment {
                    phase: AssignmentPhase::Executing,
                    ..
                }
            )
        {
            return Err(format!("Assignment {id} is not executing."));
        }
        if !is_assignment && whip.fires >= whip.max_fires {
            self.mark_whip_terminal(id, WhipState::Exhausted, "max fires reached");
            return Err(format!("Whip {id} is exhausted."));
        }
        if !matches!(trigger, FireTrigger::Manual | FireTrigger::Test) {
            let has_pending_completion = is_assignment
                && whip
                    .last_target_output
                    .as_ref()
                    .is_some_and(|output| !output.trim().is_empty())
                && whip.last_idle_generation_fired != Some(idle_generation);
            let is_assignment_completion = is_assignment
                && (matches!(trigger, FireTrigger::Completion)
                    || (matches!(trigger, FireTrigger::Tick) && has_pending_completion));
            if is_assignment_completion
                && self.assignment_result_is_delivered_by_native_parent(&whip)
            {
                return Err(format!(
                    "Assignment {id} result is delivered directly to its native Manager."
                ));
            }
            if (!is_assignment || is_assignment_completion)
                && whip.last_idle_generation_fired == Some(idle_generation)
            {
                return Err(format!("Whip {id} already fired for this idle period."));
            }
            if is_assignment_completion {
                if let Some(last_fire) = whip.last_fire_utc
                    && now - last_fire < Duration::seconds(MIN_ASSIGNMENT_COMPLETION_INTERVAL_S)
                {
                    return Err(format!(
                        "Assignment {id} is rate-limiting completion handoffs."
                    ));
                }
            } else {
                let cadence_s = assignment_effective_cadence_s(&whip);
                if let Some(last_fire) = whip.last_fire_utc
                    && now - last_fire < Duration::seconds(cadence_s as i64)
                {
                    return Err(format!("Whip {id} is inside cooldown."));
                }
            }
            if let WhipKind::Assignment {
                last_user_turn_utc: Some(last_user_turn),
                ..
            } = whip.kind
                && now - last_user_turn < Duration::seconds(whip.cooldown_s as i64)
            {
                return Err(format!(
                    "Assignment {id} is yielding to recent user activity."
                ));
            }
        }
        if !self.target_node_is_idle(&whip.target) {
            return Err(format!("Whip target `{}` is not idle.", whip.target));
        }
        let instruction = if whip.instructions == DRAFT_WITH_MANAGER_SPEC {
            Err("drafted with Manager".to_string())
        } else {
            read_whip_instruction(
                self.config.codex_home.as_ref(),
                &self.config.cwd,
                &whip.instructions,
            )
        };
        let destination_node = if is_assignment {
            let manager = whip
                .holder
                .clone()
                .ok_or_else(|| format!("Assignment {id} has no Manager."))?;
            if !matches!(trigger, FireTrigger::Test) && !self.target_node_is_idle(&manager) {
                return Err(format!("Assignment Manager `{manager}` is not idle."));
            }
            manager
        } else {
            match whip.mode {
                WhipMode::Auto => whip.target.clone(),
                WhipMode::Review => {
                    let Some(holder) = whip.holder.clone() else {
                        return Err(format!("Whip {id} has no holder."));
                    };
                    if !matches!(trigger, FireTrigger::Test) && !self.target_node_is_idle(&holder) {
                        return Err(format!("Whip holder `{holder}` is not idle."));
                    }
                    holder
                }
            }
        };
        let destination = self.fire_destination_for_node(&destination_node)?;
        let destination_label = self.node_label(&destination_node);
        let target_label = self.node_label(&whip.target);
        let task = if is_assignment {
            assignment_mandate_task(
                &whip,
                &target_label,
                instruction.as_ref().ok().map(|(_, text)| text.as_str()),
                instruction.as_ref().ok().map(|(path, _)| path.as_path()),
                now,
                self.assignment_dispatch_protocol_for_target(&whip.target),
            )
        } else {
            let (instruction_path, instruction_text) = instruction?;
            match whip.mode {
                WhipMode::Auto => auto_whip_task(&whip, &instruction_text, &instruction_path),
                WhipMode::Review => {
                    review_whip_task(&whip, &target_label, &instruction_text, &instruction_path)
                }
            }
        };
        Ok(FirePlan {
            whip_id: id.to_string(),
            mode: whip.mode,
            destination,
            task,
            target_idle_generation: idle_generation,
            destination_label,
        })
    }

    fn assignment_result_is_delivered_by_native_parent(&self, whip: &Whip) -> bool {
        let Some(holder_node_id) = whip.holder.as_deref() else {
            return false;
        };
        let Some(worker_thread_id) = node_id_thread(&whip.target) else {
            return false;
        };
        node_id_thread(holder_node_id).is_some()
            && self
                .logical_parent_node_for_thread(worker_thread_id)
                .as_deref()
                == Some(holder_node_id)
    }

    fn execute_whip_fire(&mut self, plan: FirePlan, trigger: FireTrigger) {
        let now = self.orchestrate_now();
        let (fires, max_fires, exhausted) = {
            let Some(whip) = self.orchestrate_whips.get_mut(&plan.whip_id) else {
                return;
            };
            if !matches!(trigger, FireTrigger::Test) {
                whip.fires = whip.fires.saturating_add(1);
                whip.last_fire_utc = Some(now);
                whip.last_idle_generation_fired = Some(plan.target_idle_generation);
            }
            let exhausted = !whip.is_assignment() && whip.fires >= whip.max_fires;
            if exhausted && !matches!(trigger, FireTrigger::Test) {
                whip.state = WhipState::Exhausted;
                whip.expiry_notified = true;
            }
            if plan.mode == WhipMode::Review
                && !whip.is_assignment()
                && !matches!(trigger, FireTrigger::Test)
            {
                whip.pending_review_fire = Some(whip.fires);
            }
            (whip.fires, whip.max_fires, exhausted)
        };
        match plan.destination {
            FireDestination::Native(thread_id) => {
                self.send_native_task_event(thread_id, plan.task);
            }
            FireDestination::ClaudePane(pane_id) => {
                let sent_while_kill_switch_on = self.kill_switch_on_now();
                self.app_event_tx.send(AppEvent::SubmitSpawnClaudePaneTask {
                    pane_id,
                    task: plan.task,
                    sent_while_kill_switch_on,
                });
            }
        }
        self.persist_pane_state();
        let message = if self
            .orchestrate_whips
            .get(&plan.whip_id)
            .is_some_and(Whip::is_assignment)
        {
            format!(
                "Assignment {} mandated Manager {}.",
                plan.whip_id, plan.destination_label
            )
        } else {
            format!(
                "Whip {} fired to {} ({}/{}).",
                plan.whip_id, plan.destination_label, fires, max_fires
            )
        };
        self.chat_widget.add_info_message(message, /*hint*/ None);
        if exhausted && !matches!(trigger, FireTrigger::Test) {
            self.chat_widget.add_info_message(
                format!("Whip {} exhausted: max fires reached.", plan.whip_id),
                /*hint*/ None,
            );
        }
    }

    fn pause_matching_whips_on_stop_marker(
        &mut self,
        target_node_id: &str,
        last_output: Option<&str>,
    ) {
        let Some(output) = last_output else {
            return;
        };
        let legacy_ids: Vec<String> = self
            .orchestrate_whips
            .values()
            .filter(|whip| {
                !whip.is_assignment()
                    && whip.target == target_node_id
                    && whip.state == WhipState::Armed
                    && output.contains(&whip.stop_marker)
            })
            .map(|whip| whip.id.clone())
            .collect();
        for id in legacy_ids {
            self.mark_whip_terminal(&id, WhipState::Paused, "stop marker seen");
        }

        let manager_node_id = normalize_orchestrate_node_id(target_node_id);
        let assignment_ids: Vec<String> = self
            .orchestrate_whips
            .values()
            .filter(|whip| {
                whip.is_assignment()
                    && whip.holder.as_deref() == Some(manager_node_id.as_str())
                    && whip.state == WhipState::Armed
                    && matches!(
                        whip.kind,
                        WhipKind::Assignment {
                            phase: AssignmentPhase::Executing,
                            ..
                        }
                    )
            })
            .map(|whip| whip.id.clone())
            .collect();
        for id in assignment_ids {
            let mut changed = false;
            if assignment_done_marker(output) {
                if let Some(whip) = self.orchestrate_whips.get_mut(&id)
                    && let WhipKind::Assignment { phase, .. } = &mut whip.kind
                {
                    *phase = AssignmentPhase::Done;
                    changed = true;
                }
                self.chat_widget.add_info_message(
                    format!("Assignment {id} completed by its Manager."),
                    /*hint*/ None,
                );
            } else if let Some(reason) = assignment_blocked_reason(output) {
                if let Some(whip) = self.orchestrate_whips.get_mut(&id)
                    && let WhipKind::Assignment { phase, .. } = &mut whip.kind
                {
                    *phase = AssignmentPhase::Blocked {
                        reason: reason.clone(),
                    };
                    changed = true;
                }
                self.chat_widget.add_info_message(
                    format!("Assignment {id} blocked: {reason}"),
                    /*hint*/ None,
                );
            }
            if changed {
                self.persist_pane_state();
            }
        }
    }

    fn pause_spinning_whips_on_empty_output(
        &mut self,
        target_node_id: &str,
        last_output: Option<&str>,
    ) {
        let Some(output) = last_output else {
            return;
        };
        let mut paused = Vec::new();
        for whip in self.orchestrate_whips.values_mut().filter(|whip| {
            !whip.is_assignment() && whip.target == target_node_id && whip.state == WhipState::Armed
        }) {
            if output.trim().is_empty() {
                whip.empty_output_fires = whip.empty_output_fires.saturating_add(1);
            } else {
                whip.empty_output_fires = 0;
            }
            if whip.empty_output_fires >= 2 {
                paused.push(whip.id.clone());
            }
        }
        for id in paused {
            self.mark_whip_terminal(&id, WhipState::Paused, "empty output loop");
        }
    }

    fn recover_assignment_manager_on_empty_output(
        &mut self,
        target_node_id: &str,
        last_output: Option<&str>,
        turn_succeeded: bool,
    ) {
        let node_id = normalize_orchestrate_node_id(target_node_id);
        let has_visible_output = last_output.is_some_and(|output| !output.trim().is_empty());
        let mut retry_ids = Vec::new();
        let mut pause_ids = Vec::new();
        let mut changed = false;

        for whip in self.orchestrate_whips.values_mut().filter(|whip| {
            whip.is_assignment()
                && whip.state == WhipState::Armed
                && whip.holder.as_deref() == Some(node_id.as_str())
        }) {
            if has_visible_output {
                if whip.empty_output_fires != 0 {
                    whip.empty_output_fires = 0;
                    changed = true;
                }
                continue;
            }
            // Provider failures have their own bounded retry/backoff path. This guard handles the
            // distinct failure mode where the provider reports success but emits no assistant item.
            if !turn_succeeded {
                continue;
            }
            whip.empty_output_fires = whip.empty_output_fires.saturating_add(1);
            changed = true;
            if whip.empty_output_fires == 1 {
                retry_ids.push(whip.id.clone());
            } else {
                pause_ids.push(whip.id.clone());
            }
        }

        if changed {
            self.persist_pane_state();
        }
        for id in retry_ids {
            let Some(whip) = self.orchestrate_whips.get(&id) else {
                continue;
            };
            let Some(manager) = whip.holder.as_deref() else {
                continue;
            };
            let task = format!(
                "Assignment {id} recovery: your previous turn completed successfully but emitted no visible assistant response. Process the latest user message already present in this conversation. Continue drafting the assignment from the available context and dispatch the Worker when the specification is sufficiently concrete. Do not ask the user to repeat information they already supplied."
            );
            match self.fire_destination_for_node(manager) {
                Ok(FireDestination::Native(thread_id)) => {
                    self.send_native_task_event(thread_id, task)
                }
                Ok(FireDestination::ClaudePane(pane_id)) => {
                    let sent_while_kill_switch_on = self.kill_switch_on_now();
                    self.app_event_tx.send(AppEvent::SubmitSpawnClaudePaneTask {
                        pane_id,
                        task,
                        sent_while_kill_switch_on,
                    });
                }
                Err(err) => {
                    pause_ids.push(id.clone());
                    self.chat_widget.add_error_message(format!(
                        "Assignment {id} could not retry its empty Manager turn: {err}"
                    ));
                    continue;
                }
            }
            self.chat_widget.add_info_message(
                format!(
                    "Assignment {id} Manager returned no visible response; retrying once with the existing conversation context."
                ),
                /*hint*/ None,
            );
        }
        pause_ids.sort();
        pause_ids.dedup();
        for id in pause_ids {
            self.mark_whip_terminal(
                &id,
                WhipState::Paused,
                "Manager completed twice without visible output",
            );
        }
    }

    fn pause_spinning_whips_on_failed_turn(&mut self, target_node_id: &str, turn_succeeded: bool) {
        let mut paused = Vec::new();
        let mut failing_assignments = Vec::new();
        let mut assignment_changed = false;
        let node_id = normalize_orchestrate_node_id(target_node_id);
        for whip in self
            .orchestrate_whips
            .values_mut()
            .filter(|whip| whip.state == WhipState::Armed)
        {
            if whip.is_assignment() {
                if whip.holder.as_deref() != Some(node_id.as_str()) {
                    continue;
                }
                if turn_succeeded {
                    whip.consecutive_failed_turns = 0;
                    assignment_changed = true;
                    if let WhipKind::Assignment {
                        failure_backoff_level,
                        ..
                    } = &mut whip.kind
                    {
                        *failure_backoff_level = 0;
                    }
                } else {
                    whip.consecutive_failed_turns = whip.consecutive_failed_turns.saturating_add(1);
                    assignment_changed = true;
                    if let WhipKind::Assignment {
                        failure_backoff_level,
                        ..
                    } = &mut whip.kind
                    {
                        *failure_backoff_level = failure_backoff_level.saturating_add(1).min(3);
                    }
                    if whip.consecutive_failed_turns == 3 {
                        failing_assignments.push(whip.id.clone());
                    }
                }
                continue;
            }
            if whip.target != target_node_id {
                continue;
            }
            if turn_succeeded {
                whip.consecutive_failed_turns = 0;
            } else {
                whip.consecutive_failed_turns = whip.consecutive_failed_turns.saturating_add(1);
            }
            if whip.consecutive_failed_turns >= 2 {
                paused.push(whip.id.clone());
            }
        }
        for id in paused {
            self.mark_whip_terminal(&id, WhipState::Paused, "two consecutive failed turns");
        }
        for id in failing_assignments {
            self.chat_widget.add_info_message(
                format!("Assignment {id}: Manager failing, retrying with backoff."),
                /*hint*/ None,
            );
        }
        if assignment_changed {
            self.persist_pane_state();
        }
    }

    fn note_whip_holder_idle(&mut self, holder_node_id: &str) {
        let holder_node_id = normalize_orchestrate_node_id(holder_node_id);
        let mut pause = Vec::new();
        for whip in self.orchestrate_whips.values_mut().filter(|whip| {
            !whip.is_assignment()
                && whip.mode == WhipMode::Review
                && whip.state == WhipState::Armed
                && whip.holder.as_deref() == Some(holder_node_id.as_str())
                && whip.pending_review_fire.is_some()
        }) {
            whip.pending_review_fire = None;
            whip.ignored_review_fires = whip.ignored_review_fires.saturating_add(1);
            if whip.ignored_review_fires >= 2 {
                pause.push(whip.id.clone());
            }
        }
        for id in pause {
            self.mark_whip_terminal(&id, WhipState::Paused, "holder ignored two review fires");
        }

        // A Worker completion can arrive while its Manager is still handling an earlier
        // mandate. Its idle generation remains unaudited until the Manager becomes idle.
        let pending_reports: Vec<(String, u64)> = self
            .orchestrate_whips
            .values()
            .filter(|whip| {
                whip.is_assignment()
                    && whip.state == WhipState::Armed
                    && matches!(
                        whip.kind,
                        WhipKind::Assignment {
                            phase: AssignmentPhase::Executing,
                            ..
                        }
                    )
                    && whip.holder.as_deref() == Some(holder_node_id.as_str())
                    && whip
                        .last_target_output
                        .as_ref()
                        .is_some_and(|output| !output.trim().is_empty())
            })
            .filter_map(|whip| {
                let generation = self
                    .orchestrate_idle_generation_by_target
                    .get(&whip.target)
                    .copied()
                    .unwrap_or(0);
                (generation > 0 && whip.last_idle_generation_fired != Some(generation))
                    .then_some((whip.target.clone(), generation))
            })
            .collect();
        for (target, generation) in pending_reports {
            self.evaluate_whips_for_target(&target, generation, FireTrigger::Completion);
        }
    }

    fn expire_whip_if_needed(&mut self, id: &str, now: DateTime<Utc>) {
        let should_expire = self.orchestrate_whips.get(id).is_some_and(|whip| {
            whip.state == WhipState::Armed
                && !matches!(
                    whip.kind,
                    WhipKind::Assignment {
                        phase: AssignmentPhase::Done,
                        ..
                    }
                )
                && whip.expires_at.is_some_and(|expires_at| expires_at <= now)
        });
        if should_expire {
            self.mark_whip_terminal(id, WhipState::Expired, "expired");
        }
    }

    fn authorize_orchestrate_command(
        &self,
        command: &OrchestrateCommand,
        origin: CommandOrigin<'_>,
    ) -> Result<(), String> {
        let CommandOrigin::Agent(agent_node) = origin else {
            return Ok(());
        };
        let agent_node = normalize_orchestrate_node_id(agent_node);
        match command {
            OrchestrateCommand::Status => Ok(()),
            OrchestrateCommand::Pause(id_or_target) | OrchestrateCommand::Detach(id_or_target) => {
                self.ensure_agent_controls_whip(id_or_target, &agent_node)
            }
            OrchestrateCommand::Extend { id, .. } => {
                self.ensure_agent_controls_whip(id, &agent_node)
            }
            OrchestrateCommand::Attach { target, expiry, .. } => {
                self.ensure_agent_attach_expiry_allowed(*expiry)?;
                let target_node_id = self.resolve_orchestrate_target_node(target)?;
                for whip in self.orchestrate_whips.values().filter(|whip| {
                    whip.target == target_node_id && whip.state != WhipState::Detached
                }) {
                    match whip.holder.as_deref() {
                        Some(holder) if holder == agent_node => {}
                        Some(holder) => {
                            return Err(format!(
                                "Agent `{agent_node}` cannot replace whip {} held by `{holder}`.",
                                whip.id
                            ));
                        }
                        None => {
                            return Err(format!(
                                "Agent `{agent_node}` cannot replace user-owned holderless whip {}.",
                                whip.id
                            ));
                        }
                    }
                }
                Ok(())
            }
            OrchestrateCommand::Resume(_)
            | OrchestrateCommand::Start(_)
            | OrchestrateCommand::Fire(_)
            | OrchestrateCommand::Test(_) => {
                Err("Only the user can resume, fire, or test whips.".to_string())
            }
        }
    }

    fn ensure_agent_controls_whip(
        &self,
        id_or_target: &str,
        agent_node: &str,
    ) -> Result<(), String> {
        let id = self
            .find_whip_id(id_or_target)
            .ok_or_else(|| format!("No whip found for `{id_or_target}`."))?;
        let Some(whip) = self.orchestrate_whips.get(&id) else {
            return Err(format!("No whip found for `{id}`."));
        };
        if whip.holder.as_deref() == Some(agent_node) || whip.target == agent_node {
            return Ok(());
        }
        Err(format!(
            "Agent `{agent_node}` cannot control whip {id}; it neither holds nor targets that whip."
        ))
    }

    fn ensure_agent_attach_expiry_allowed(&self, expiry: Option<ExpiryArg>) -> Result<(), String> {
        let Some(expiry) = expiry else {
            return Ok(());
        };
        match expiry {
            ExpiryArg::Unlimited => Err("Agent-origin whips cannot be unlimited.".to_string()),
            ExpiryArg::Duration(duration) if duration.seconds > DEFAULT_EXPIRY_SECONDS => {
                Err("Agent-origin whips cannot request a duration longer than 4h.".to_string())
            }
            ExpiryArg::UntilTodayOrTomorrow { .. } => {
                let now = self.orchestrate_now();
                let Some(expires_at) = resolve_expiry_arg(expiry, now)? else {
                    return Err("Agent-origin whips cannot be unlimited.".to_string());
                };
                if expires_at - now > Duration::seconds(DEFAULT_EXPIRY_SECONDS) {
                    return Err(
                        "Agent-origin whips cannot request a duration longer than 4h.".to_string(),
                    );
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn mark_whip_terminal(&mut self, id: &str, state: WhipState, reason: &str) {
        let Some(whip) = self.orchestrate_whips.get_mut(id) else {
            return;
        };
        if whip.state == state && whip.expiry_notified {
            return;
        }
        let is_assignment = whip.is_assignment();
        whip.state = state;
        whip.expiry_notified = true;
        self.persist_pane_state();
        let subject = if is_assignment { "Assignment" } else { "Whip" };
        self.chat_widget.add_info_message(
            format!("{subject} {id} {}: {reason}.", state.label()),
            /*hint*/ None,
        );
    }

    fn resolve_orchestrate_target_node(&self, target: &str) -> Result<String, String> {
        if (target == CODEX_MAIN_PANE_ID || target == pane_node_id(CODEX_MAIN_PANE_ID))
            && let Some(thread_id) = self.primary_thread_id
        {
            return Ok(thread_node_id(thread_id));
        }
        match self.resolve_spawn_task_target(target)? {
            SpawnTaskTarget::Native(thread_id) | SpawnTaskTarget::UnavailableNative(thread_id) => {
                Ok(thread_node_id(thread_id))
            }
            SpawnTaskTarget::ClaudePane(pane_id) => Ok(pane_node_id(&pane_id)),
        }
    }

    fn fire_destination_for_node(&self, node_id: &str) -> Result<FireDestination, String> {
        if let Some(thread_id) = node_id_thread(node_id) {
            if std::env::var("PFTERMINAL_ORCHESTRATE_QA").as_deref() == Ok("1")
                && std::env::var("PFTERMINAL_ORCHESTRATE_QA_CONTROL")
                    .ok()
                    .and_then(|path| fs::read_to_string(path).ok())
                    .is_some_and(|control| {
                        control.lines().any(|line| {
                            line.trim() == format!("unavailable={}", thread_node_id(thread_id))
                        })
                    })
            {
                return Err(format!("Native pane `{thread_id}` is unavailable."));
            }
            return self
                .native_thread_loaded_for_orchestrate(thread_id)
                .then_some(FireDestination::Native(thread_id))
                .ok_or_else(|| format!("Native pane `{thread_id}` is not loaded."));
        }
        if let Some(pane_id) = node_id_pane(node_id) {
            if pane_id == CODEX_MAIN_PANE_ID {
                return self
                    .primary_thread_id
                    .map(FireDestination::Native)
                    .ok_or_else(|| "Corbanu Terminal Main is not loaded.".to_string());
            }
            if self
                .claude_panes
                .panes()
                .iter()
                .any(|pane| pane.id == pane_id)
            {
                // Refused before the fire is counted or reported as started.
                if let Some(reason) = crate::security::level::external_agent_block_reason() {
                    return Err(reason);
                }
                return Ok(FireDestination::ClaudePane(pane_id.to_string()));
            }
        }
        Err(format!("Whip destination `{node_id}` is not loaded."))
    }

    fn current_holder_node(&self) -> Result<String, String> {
        let active_pane = self.claude_panes.active_user_pane_id();
        if active_pane != CODEX_MAIN_PANE_ID {
            return Ok(pane_node_id(active_pane));
        }
        self.active_thread_id
            .or(self.primary_thread_id)
            .map(thread_node_id)
            .ok_or_else(|| {
                "No current Corbanu Terminal pane is available as whip holder.".to_string()
            })
    }

    fn target_node_is_idle(&self, node_id: &str) -> bool {
        if let Some(thread_id) = node_id_thread(node_id) {
            if self.primary_thread_id == Some(thread_id) {
                return self.native_thread_idle_for_orchestrate(thread_id);
            }
            return self
                .agent_navigation
                .get(&thread_id)
                .is_some_and(|entry| !entry.is_running && !entry.is_closed);
        }
        if let Some(pane_id) = node_id_pane(node_id) {
            if pane_id == CODEX_MAIN_PANE_ID {
                return self
                    .primary_thread_id
                    .and_then(|thread_id| self.agent_navigation.get(&thread_id))
                    .is_some_and(|entry| !entry.is_running && !entry.is_closed);
            }
            return self
                .claude_panes
                .panes()
                .iter()
                .find(|pane| pane.id == pane_id)
                .is_some_and(|pane| pane.status != crate::claude_panes::ClaudePaneStatus::Running);
        }
        false
    }

    fn node_label(&self, node_id: &str) -> String {
        self.spawn_node_title(node_id)
            .unwrap_or_else(|| node_id.to_string())
    }

    fn next_orchestrate_id(&mut self, assignment: bool) -> String {
        self.orchestrate_next_whip_seq = self.orchestrate_next_whip_seq.saturating_add(1);
        let prefix = if assignment { "assignment" } else { "whip" };
        format!("{prefix}-{}", self.orchestrate_next_whip_seq)
    }

    fn find_whip_id(&self, id_or_target: &str) -> Option<String> {
        if self.orchestrate_whips.contains_key(id_or_target) {
            return Some(id_or_target.to_string());
        }
        let resolved_target = self.resolve_orchestrate_target_node(id_or_target).ok();
        self.orchestrate_whips
            .values()
            .filter(|whip| whip.state != WhipState::Detached)
            .find(|whip| {
                resolved_target
                    .as_ref()
                    .is_some_and(|target| whip.target == *target)
                    || whip.target == id_or_target
            })
            .map(|whip| whip.id.clone())
    }
}

fn assignment_user_facing_dispatch_cause(cause: &str) -> String {
    cause
        .replace("Target", "Destination")
        .replace("target", "destination")
}

#[derive(Debug, Clone, Copy)]
enum CommandOrigin<'a> {
    User,
    Agent(&'a str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FireTrigger {
    Edge,
    Completion,
    Tick,
    Manual,
    Test,
}

pub(crate) fn assignment_effective_cadence_s(whip: &Whip) -> u64 {
    let level = match whip.kind {
        WhipKind::Assignment {
            failure_backoff_level,
            ..
        } => failure_backoff_level.min(3),
        WhipKind::LegacyNudge => return whip.cooldown_s,
    };
    whip.cooldown_s
        .saturating_mul(1_u64 << level)
        .min(2 * 60 * 60)
}

pub(crate) fn assignment_mandate_task(
    whip: &Whip,
    worker_label: &str,
    instructions: Option<&str>,
    path: Option<&Path>,
    now: DateTime<Utc>,
    dispatch_protocol: AssignmentDispatchProtocol,
) -> String {
    let source = path
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "drafted with Manager".to_string());
    let spec = instructions
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| format!("\n\nAssignment spec:\n{text}"))
        .unwrap_or_default();
    let worker_result = whip
        .last_target_output
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| format!("\n\nWorker's latest completed output:\n{text}"))
        .unwrap_or_else(|| "\n\nWorker's latest completed output: unavailable.".to_string());
    let dispatch_protocol =
        assignment_dispatch_instructions(whip, "<next concrete task>", dispatch_protocol);
    format!(
        "Assignment {} mandate. Worker {worker_label} stopped at {}. Audit its latest result and act.\n\n{dispatch_protocol}\n\nReport ASSIGNMENT_BLOCKED: <reason> only for a genuinely user-owned decision, or emit WHIP_DONE only when complete. When you emit either marker, place it alone on its own line. Spec source: {source}.{spec}{worker_result}",
        whip.id,
        now.to_rfc3339(),
    )
}

fn assignment_birth_brief(
    whip: &Whip,
    manager_label: &str,
    worker_label: &str,
    instructions: Option<&str>,
    path: Option<&Path>,
    dispatch_protocol: AssignmentDispatchProtocol,
) -> String {
    let duration = match whip.kind {
        WhipKind::Assignment {
            execution_duration_s,
            ..
        } => execution_duration_s
            .map(format_duration_seconds)
            .unwrap_or_else(|| "ended by the user".to_string()),
        WhipKind::LegacyNudge => "the configured duration".to_string(),
    };
    let source = path
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "Draft with Manager".to_string());
    let spec_is_locked = instructions.is_some();
    let inline = instructions
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| format!("\n\nInitial assignment spec ({source}):\n{text}"))
        .unwrap_or_else(|| format!("\n\nSpec source: {source}."));
    let kickoff = if spec_is_locked {
        format!(
            "The spec below is locked. Send the first concrete Worker task now to `{}` without changing that target. Do not ask for approval or do the Worker's task yourself. After sending it, wait for the Worker result.",
            whip.target,
        )
    } else {
        format!(
            "Draft mode: ask the user for the assignment requirements. When they are concrete, send the first task to Worker {worker_label}."
        )
    };
    let dispatch_protocol =
        assignment_dispatch_instructions(whip, "<concrete task>", dispatch_protocol);
    format!(
        "You are Manager {manager_label} for Worker {worker_label}. This assignment lasts until {duration} after execution starts.\n\n{dispatch_protocol}\n\n{kickoff} After each Worker result, audit it and send the next task until the assignment is complete. Keep progress concise. User messages take priority. Use WHIP_DONE alone only when complete, or ASSIGNMENT_BLOCKED: <reason> alone only for a decision only the user can make. If the Worker is idle, you will be prompted again after {} minutes.{inline}",
        whip.cooldown_s / 60,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AssignmentDispatchProtocol {
    NativeFollowup,
    HostAdapter,
}

fn assignment_dispatch_instructions(
    whip: &Whip,
    task_placeholder: &str,
    dispatch_protocol: AssignmentDispatchProtocol,
) -> String {
    match dispatch_protocol {
        AssignmentDispatchProtocol::NativeFollowup => {
            let worker_thread_id = node_id_thread(&whip.target)
                .map(|thread_id| thread_id.to_string())
                .unwrap_or_else(|| whip.target.clone());
            format!(
                "Dispatch only with the native `followup_task` collaboration tool. Set its target to the durable Worker thread ID `{worker_thread_id}` and its message to `{task_placeholder}`. This is not a shell command or tool-discovery request. Do not emit a `pfterminal-send-task` block, spawn another agent, or replace the Worker."
            )
        }
        AssignmentDispatchProtocol::HostAdapter => format!(
            "To send work across this host-routed pane boundary, write this fenced assistant-text block; it is not a shell command or tool:\n```pfterminal-send-task\n{{\"target\":\"{}\",\"task\":\"{task_placeholder}\"}}\n```",
            whip.target,
        ),
    }
}

fn format_duration_seconds(seconds: i64) -> String {
    let (count, unit) = if seconds % 3600 == 0 {
        (seconds / 3600, "hour")
    } else if seconds % 60 == 0 {
        (seconds / 60, "minute")
    } else {
        (seconds, "second")
    };
    if count == 1 {
        format!("1 {unit}")
    } else {
        format!("{count} {unit}s")
    }
}

fn assignment_blocked_reason(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let reason = line.trim().strip_prefix("ASSIGNMENT_BLOCKED:")?;
        let reason = reason.trim();
        (!reason.is_empty()).then(|| reason.to_string())
    })
}

fn assignment_done_marker(output: &str) -> bool {
    output
        .lines()
        .any(|line| line.trim() == DEFAULT_STOP_MARKER)
}

fn assignment_phase_label(phase: &AssignmentPhase) -> String {
    match phase {
        AssignmentPhase::Drafting => "drafting".to_string(),
        AssignmentPhase::Executing => "executing".to_string(),
        AssignmentPhase::Blocked { reason } => format!("blocked ({reason})"),
        AssignmentPhase::Done => "done".to_string(),
    }
}

fn assignment_expiry_label(whip: &Whip, now: DateTime<Utc>) -> String {
    if let WhipKind::Assignment {
        phase: AssignmentPhase::Drafting,
        execution_duration_s,
        ..
    } = &whip.kind
    {
        return execution_duration_s
            .map(|seconds| format!("{} after start", format_duration_seconds(seconds)))
            .unwrap_or_else(|| "unlimited".to_string());
    }
    match whip.expires_at {
        Some(expires_at) if expires_at <= now => "expired".to_string(),
        Some(expires_at) => format!("expires {}", expires_at.format("%H:%MZ")),
        None => "unlimited".to_string(),
    }
}

fn auto_whip_task(whip: &Whip, instructions: &str, path: &Path) -> String {
    format!(
        "Whip #{} fire {}/{} ({})\nInstruction source: {}\n\n{}",
        whip.id,
        whip.fires.saturating_add(1),
        whip.max_fires,
        Utc::now().to_rfc3339(),
        path.display(),
        instructions.trim()
    )
}

fn normalize_orchestrate_node_id(node_or_pane_id: &str) -> String {
    if node_or_pane_id.starts_with("thread:") || node_or_pane_id.starts_with("pane:") {
        node_or_pane_id.to_string()
    } else {
        pane_node_id(node_or_pane_id)
    }
}

fn review_whip_task(whip: &Whip, target_label: &str, instructions: &str, path: &Path) -> String {
    format!(
        "Whip-review turn for {}.\nTarget: {}\nWhip document: {}\nFire budget: {}/{}\nTime left: {}\n\nTarget is idle. Review the target's last result and decide the next directive. If more work is needed, dispatch it through the normal pfterminal-send-task block to the target. If done, emit a pfterminal-orchestrate block to pause or detach this whip.\n\nWhip instructions:\n{}",
        whip.id,
        target_label,
        path.display(),
        whip.fires.saturating_add(1),
        whip.max_fires,
        whip.expires_at
            .map(|expires_at| expires_at.to_rfc3339())
            .unwrap_or_else(|| "unlimited".to_string()),
        instructions.trim()
    )
}

fn parse_holder_arg(value: &str) -> HolderArg {
    if value.eq_ignore_ascii_case("me") {
        HolderArg::Me
    } else if value.eq_ignore_ascii_case("none") {
        HolderArg::None
    } else {
        HolderArg::Target(value.trim().to_string())
    }
}

fn one_arg<'a>(action: &str, mut parts: impl Iterator<Item = &'a str>) -> Result<String, String> {
    let value = parts
        .next()
        .ok_or_else(|| format!("Usage: /orchestrate {action} <id|target>"))?
        .to_string();
    if parts.next().is_some() {
        return Err(format!("Usage: /orchestrate {action} <id|target>"));
    }
    Ok(value)
}

fn parse_positive_u32(value: &str, flag: &str) -> Result<u32, String> {
    let parsed = value
        .parse::<u32>()
        .map_err(|_| format!("Invalid value for {flag}: `{value}`."))?;
    if parsed == 0 {
        return Err(format!("{flag} must be greater than zero."));
    }
    Ok(parsed)
}

fn parse_duration_arg(value: &str) -> Result<DurationArg, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("Duration cannot be empty.".to_string());
    }
    let (number, multiplier) = match trimmed.chars().last().unwrap_or_default() {
        's' | 'S' => (&trimmed[..trimmed.len() - 1], 1),
        'm' | 'M' => (&trimmed[..trimmed.len() - 1], 60),
        'h' | 'H' => (&trimmed[..trimmed.len() - 1], 60 * 60),
        'd' | 'D' => (&trimmed[..trimmed.len() - 1], 24 * 60 * 60),
        _ => (trimmed, 1),
    };
    let amount = number
        .parse::<i64>()
        .map_err(|_| format!("Invalid duration `{value}`."))?;
    if amount <= 0 {
        return Err("Duration must be greater than zero.".to_string());
    }
    Ok(DurationArg {
        seconds: amount.saturating_mul(multiplier),
    })
}

fn parse_until_arg(value: &str) -> Result<ExpiryArg, String> {
    let parsed = NaiveTime::parse_from_str(value, "%H:%M")
        .map_err(|_| "Expected --until HH:MM in UTC.".to_string())?;
    Ok(ExpiryArg::UntilTodayOrTomorrow {
        hour: parsed.hour(),
        minute: parsed.minute(),
    })
}

fn resolve_expiry_arg(
    value: ExpiryArg,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, String> {
    match value {
        ExpiryArg::Duration(duration) => Ok(Some(now + Duration::seconds(duration.seconds))),
        ExpiryArg::Unlimited => Ok(None),
        ExpiryArg::UntilTodayOrTomorrow { hour, minute } => {
            let today = now.date_naive();
            let Some(naive_time) = NaiveTime::from_hms_opt(hour, minute, 0) else {
                return Err("Invalid --until time.".to_string());
            };
            let mut expiry = today.and_time(naive_time).and_utc();
            if expiry <= now {
                expiry += Duration::days(1);
            }
            Ok(Some(expiry))
        }
    }
}

fn parse_whip_doc_defaults(text: &str) -> WhipDocDefaults {
    let mut defaults = WhipDocDefaults {
        mode: None,
        max_fires: None,
        cooldown_s: None,
        stop_marker: None,
    };
    let trimmed = text.trim_start();
    if !trimmed.starts_with("---") {
        return defaults;
    }
    let mut lines = trimmed.lines();
    let _ = lines.next();
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            match key.trim().to_ascii_lowercase().as_str() {
                "mode" => defaults.mode = WhipMode::parse(value).ok(),
                "max_fires" | "max" => defaults.max_fires = value.trim().parse::<u32>().ok(),
                "cooldown_s" | "cooldown" => {
                    defaults.cooldown_s = parse_duration_arg(value)
                        .ok()
                        .map(|duration| duration.seconds.max(0) as u64)
                }
                "stop_marker" => defaults.stop_marker = Some(value.trim().to_string()),
                _ => {}
            }
        }
    }
    defaults
}

fn yamlish_fields(content: &str) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    for line in content.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        fields.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignment_kind_round_trips_and_legacy_json_defaults() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-10T20:00:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);
        let mut whip = Whip::new(
            "whip-1".to_string(),
            Some("pane:manager".to_string()),
            "pane:worker".to_string(),
            "spec".to_string(),
            ResolvedAttachOptions::default(),
            now,
        );
        whip.kind = WhipKind::Assignment {
            phase: AssignmentPhase::Blocked {
                reason: "credentials".to_string(),
            },
            execution_started_utc: Some(now),
            last_user_turn_utc: Some(now + Duration::minutes(1)),
            failure_backoff_level: 3,
            execution_duration_s: Some(28_800),
        };
        let encoded = serde_json::to_string(&whip).expect("serialize assignment");
        let decoded: Whip = serde_json::from_str(&encoded).expect("restore assignment");
        assert_eq!(decoded, whip);

        let mut legacy = serde_json::to_value(&whip).expect("legacy value");
        legacy.as_object_mut().expect("object").remove("kind");
        let decoded: Whip = serde_json::from_value(legacy).expect("restore legacy whip");
        assert_eq!(decoded.kind, WhipKind::LegacyNudge);
    }

    #[test]
    fn assignment_blocked_marker_requires_a_reason() {
        assert_eq!(
            assignment_blocked_reason("progress\nASSIGNMENT_BLOCKED: missing credentials"),
            Some("missing credentials".to_string())
        );
        assert_eq!(assignment_blocked_reason("ASSIGNMENT_BLOCKED:"), None);
        assert_eq!(
            assignment_blocked_reason("I will emit ASSIGNMENT_BLOCKED: missing credentials"),
            None
        );
        assert!(!assignment_done_marker(
            "I will emit WHIP_DONE when the work is complete."
        ));
        assert!(assignment_done_marker("progress\n  WHIP_DONE  \n"));
    }

    #[test]
    fn drafting_expiry_label_uses_deferred_execution_budget() {
        let now = Utc::now();
        let mut whip = Whip::new(
            "whip-1".to_string(),
            Some("pane:manager".to_string()),
            "pane:worker".to_string(),
            "spec".to_string(),
            ResolvedAttachOptions::default(),
            now,
        );
        whip.kind = WhipKind::Assignment {
            phase: AssignmentPhase::Drafting,
            execution_started_utc: None,
            last_user_turn_utc: Some(now),
            failure_backoff_level: 0,
            execution_duration_s: Some(8 * 60 * 60),
        };
        whip.expires_at = None;
        assert_eq!(assignment_expiry_label(&whip, now), "8 hours after start");
        if let WhipKind::Assignment {
            execution_duration_s,
            ..
        } = &mut whip.kind
        {
            *execution_duration_s = None;
        }
        assert_eq!(assignment_expiry_label(&whip, now), "unlimited");
    }

    #[test]
    fn assignment_duration_label_uses_singular_units() {
        assert_eq!(format_duration_seconds(/*seconds*/ 60 * 60), "1 hour");
        assert_eq!(format_duration_seconds(/*seconds*/ 60), "1 minute");
        assert_eq!(format_duration_seconds(/*seconds*/ 1), "1 second");
        assert_eq!(format_duration_seconds(/*seconds*/ 2 * 60 * 60), "2 hours");
        assert_eq!(format_duration_seconds(/*seconds*/ 90 * 60), "90 minutes");
        assert_eq!(format_duration_seconds(/*seconds*/ 3599), "3599 seconds");
    }

    #[test]
    fn drafting_brief_defines_native_dispatch_protocol_and_exact_target() {
        let now = Utc::now();
        let manager_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000601").expect("manager id");
        let worker_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000602").expect("worker id");
        let mut whip = Whip::new(
            "whip-1".to_string(),
            Some(thread_node_id(manager_thread_id)),
            thread_node_id(worker_thread_id),
            DRAFT_WITH_MANAGER_SPEC.to_string(),
            ResolvedAttachOptions::default(),
            now,
        );
        whip.kind = WhipKind::Assignment {
            phase: AssignmentPhase::Drafting,
            execution_started_utc: None,
            last_user_turn_utc: Some(now),
            failure_backoff_level: 0,
            execution_duration_s: Some(28_800),
        };

        let brief = assignment_birth_brief(
            &whip,
            "Manager",
            "Worker",
            /*instructions*/ None,
            /*path*/ None,
            AssignmentDispatchProtocol::NativeFollowup,
        );

        assert!(brief.contains("native `followup_task` collaboration tool"));
        assert!(brief.contains(&worker_thread_id.to_string()));
        assert!(brief.contains("Do not emit a `pfterminal-send-task` block"));
        assert!(brief.contains("not a shell command or tool-discovery request"));
        assert!(brief.contains("Draft mode: ask the user for the assignment requirements"));
        assert!(
            brief.len() < 1_500,
            "birth brief should stay concise for provider reliability"
        );
    }

    #[test]
    fn operator_pane_assignment_brief_retains_host_dispatch_adapter() {
        let now = Utc::now();
        let mut whip = Whip::new(
            "whip-1".to_string(),
            Some(thread_node_id(
                ThreadId::from_string("00000000-0000-0000-0000-000000000603").expect("manager id"),
            )),
            thread_node_id(
                ThreadId::from_string("00000000-0000-0000-0000-000000000604").expect("worker id"),
            ),
            DRAFT_WITH_MANAGER_SPEC.to_string(),
            ResolvedAttachOptions::default(),
            now,
        );
        whip.kind = WhipKind::Assignment {
            phase: AssignmentPhase::Drafting,
            execution_started_utc: None,
            last_user_turn_utc: Some(now),
            failure_backoff_level: 0,
            execution_duration_s: Some(28_800),
        };

        let brief = assignment_birth_brief(
            &whip,
            "Manager",
            "Worker",
            /*instructions*/ None,
            /*path*/ None,
            AssignmentDispatchProtocol::HostAdapter,
        );

        assert!(brief.contains("```pfterminal-send-task"));
        assert!(brief.contains("\"target\":\"thread:00000000-0000-0000-0000-000000000604\""));
        assert!(!brief.contains("native `followup_task` collaboration tool"));
    }

    #[test]
    fn draft_with_manager_name_is_reserved_for_guided_flow() {
        assert!(validate_whip_name(DRAFT_WITH_MANAGER_SPEC).is_err());
    }

    #[test]
    fn parse_attach_command_with_bounds() {
        let parsed = parse_orchestrate_command(
            "attach Krimp quant --mode auto --for 2h --max 7 --cooldown 30s --holder none",
        )
        .expect("parse");

        assert_eq!(
            parsed,
            OrchestrateCommand::Attach {
                target: "Krimp".to_string(),
                whip_name: "quant".to_string(),
                mode: Some(WhipMode::Auto),
                expiry: Some(ExpiryArg::Duration(DurationArg { seconds: 7200 })),
                max_fires: Some(7),
                cooldown_s: Some(30),
                holder: Some(HolderArg::None),
            }
        );
    }

    #[test]
    fn extracts_orchestrate_fenced_blocks_from_visible_text() {
        let text = "before\n```pfterminal-orchestrate\naction: attach\ntarget: Krimp\nwhip: quant\nmode: auto\n```\nafter";

        let (visible, blocks) = extract_orchestrate_blocks(text);

        assert_eq!(visible, "before\n\nafter");
        assert_eq!(blocks.len(), 1);
        assert!(matches!(
            &blocks[0].command,
            OrchestrateCommand::Attach { target, whip_name, mode, .. }
                if target == "Krimp" && whip_name == "quant" && *mode == Some(WhipMode::Auto)
        ));
    }

    #[test]
    fn doc_frontmatter_overrides_defaults() {
        let defaults = parse_whip_doc_defaults(
            "---\nmode: auto\nmax_fires: 3\ncooldown_s: 2m\nstop_marker: DONE\n---\n# whip: x",
        );

        assert_eq!(defaults.mode, Some(WhipMode::Auto));
        assert_eq!(defaults.max_fires, Some(3));
        assert_eq!(defaults.cooldown_s, Some(120));
        assert_eq!(defaults.stop_marker.as_deref(), Some("DONE"));
    }

    #[test]
    fn guided_attach_args_parse_to_attach_command() {
        let args = orchestrate_guided_attach_args(
            "claude-123",
            "1h",
            BUILTIN_KEEP_GOING_WHIP,
            "thread-manager",
        );
        let parsed = parse_orchestrate_command(&args).expect("parse");

        assert_eq!(
            parsed,
            OrchestrateCommand::Attach {
                target: "claude-123".to_string(),
                whip_name: BUILTIN_KEEP_GOING_WHIP.to_string(),
                mode: Some(WhipMode::Review),
                expiry: Some(ExpiryArg::Duration(DurationArg { seconds: 3600 })),
                max_fires: None,
                cooldown_s: Some(DEFAULT_ASSIGNMENT_CADENCE_S),
                holder: Some(HolderArg::Target("thread-manager".to_string())),
            }
        );
    }

    #[test]
    fn start_command_parses_for_explicit_assignment_override() {
        assert_eq!(
            parse_orchestrate_command("start whip-7").expect("parse start"),
            OrchestrateCommand::Start("whip-7".to_string())
        );
    }

    #[test]
    fn builtin_keep_going_is_available_without_file() {
        let codex_home = tempfile::tempdir().expect("codex home");
        let cwd = tempfile::tempdir().expect("cwd");

        let (path, contents) =
            read_whip_instruction(codex_home.path(), cwd.path(), BUILTIN_KEEP_GOING_WHIP)
                .expect("builtin whip");

        assert_eq!(path, PathBuf::from(BUILTIN_KEEP_GOING_PATH));
        assert_eq!(contents, BUILTIN_KEEP_GOING_INSTRUCTION);
    }

    #[test]
    fn global_keep_going_file_overrides_builtin() {
        let codex_home = tempfile::tempdir().expect("codex home");
        let cwd = tempfile::tempdir().expect("cwd");
        let global_whips = codex_home.path().join("whips");
        fs::create_dir_all(&global_whips).expect("global whips dir");
        let global_path = global_whips.join("keep-going.md");
        fs::write(&global_path, "global override").expect("write global whip");

        let (path, contents) =
            read_whip_instruction(codex_home.path(), cwd.path(), BUILTIN_KEEP_GOING_WHIP)
                .expect("global whip");

        assert_eq!(path, global_path);
        assert_eq!(contents, "global override");
    }

    #[test]
    fn corbanu_project_file_overrides_global_and_legacy_project_files() {
        let codex_home = tempfile::tempdir().expect("codex home");
        let cwd = tempfile::tempdir().expect("cwd");
        let global_whips = codex_home.path().join("whips");
        fs::create_dir_all(&global_whips).expect("global whips dir");
        fs::write(global_whips.join("keep-going.md"), "global override")
            .expect("write global whip");
        let legacy_project_whips = cwd.path().join(".pfterminal").join("whips");
        fs::create_dir_all(&legacy_project_whips).expect("legacy project whips dir");
        fs::write(
            legacy_project_whips.join("keep-going.md"),
            "legacy project override",
        )
        .expect("write legacy project whip");
        let project_whips = cwd.path().join(".corbanu").join("whips");
        fs::create_dir_all(&project_whips).expect("project whips dir");
        let project_path = project_whips.join("keep-going.md");
        fs::write(&project_path, "project override").expect("write project whip");

        let (path, contents) =
            read_whip_instruction(codex_home.path(), cwd.path(), "keep-going.md")
                .expect("project whip");

        assert_eq!(path, project_path);
        assert_eq!(contents, "project override");
    }

    #[test]
    fn legacy_project_file_remains_readable() {
        let codex_home = tempfile::tempdir().expect("codex home");
        let cwd = tempfile::tempdir().expect("cwd");
        let legacy_whips = cwd.path().join(".pfterminal").join("whips");
        fs::create_dir_all(&legacy_whips).expect("legacy project whips dir");
        let legacy_path = legacy_whips.join("legacy.md");
        fs::write(&legacy_path, "legacy instructions").expect("write legacy whip");

        let (path, contents) = read_whip_instruction(codex_home.path(), cwd.path(), "legacy")
            .expect("legacy project whip");

        assert_eq!(path, legacy_path);
        assert_eq!(contents, "legacy instructions");
    }

    #[test]
    fn available_whip_entries_include_builtin_once() {
        let codex_home = tempfile::tempdir().expect("codex home");
        let cwd = tempfile::tempdir().expect("cwd");
        let entries = available_whip_instruction_entries(codex_home.path(), cwd.path());

        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.name == BUILTIN_KEEP_GOING_WHIP)
                .count(),
            1
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.description.starts_with("built-in:"))
        );
    }
}
