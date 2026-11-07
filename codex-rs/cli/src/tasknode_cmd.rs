use anyhow::Context;
use clap::Args;
use clap::Parser;
use clap::Subcommand;
use codex_core::config::ConfigBuilder;
use codex_utils_cli::CliConfigOverrides;
use codex_utils_cli::ProfileV2Name;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::io::Write;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

const DEFAULT_TASKNODE_ORIGIN: &str = "https://tasknode.postfiat.org";

#[derive(Debug, Parser)]
pub(crate) struct TaskNodeCli {
    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    /// Use Task Node credentials owned by this Corbanu profile.
    #[arg(long = "profile", short = 'p', global = true)]
    pub config_profile: Option<ProfileV2Name>,

    /// Emit JSON. This helper always emits JSON; the flag is accepted for scripts.
    #[arg(long, global = true, default_value_t = false)]
    pub json: bool,

    /// Override Task Node origin. Defaults to PFT_TASKNODE_ORIGIN, TASKNODE_ORIGIN, saved session origin, or production.
    #[arg(long, global = true)]
    pub origin: Option<String>,

    #[command(subcommand)]
    pub command: TaskNodeCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum TaskNodeCommand {
    /// Link Task Node via GitHub, or manage a link attempt. Starting a link
    /// never disturbs an existing working session.
    Link(LinkCli),

    /// Show linked account, wallet, server flags, and task counts.
    Status,

    /// Show linked-wallet PFT balance.
    Balance(BalanceArgs),

    /// Show recent rewarded tasks.
    Rewards(RewardsCli),

    /// Work with Task Node chat.
    Chat(ChatCli),

    /// Read or save the Task Node context document.
    Context(ContextCli),

    /// Read shared collaborator context from Task Node.
    Team(TeamCli),

    /// Create a new task request.
    Request(RequestCli),

    /// List or inspect active task-generation requests.
    Requests(RequestsCli),

    /// List Task Node tasks by tab.
    Tasks(TasksCli),

    /// Inspect or mutate one Task Node task.
    Task(TaskCli),

    /// Respond to verification requests.
    Verification(VerificationCli),
}

#[derive(Debug, Args)]
pub(crate) struct LinkCli {
    #[command(subcommand)]
    action: Option<LinkCommand>,
}

#[derive(Debug, Subcommand)]
enum LinkCommand {
    /// Start a GitHub link attempt (default). Prints the verification URL.
    Start,

    /// Poll the pending attempt; on success the token is validated against the
    /// server before it replaces any stored session.
    Poll(LinkPollArgs),

    /// Show non-secret local auth state (works without a valid session).
    Status,

    /// Abandon the pending link attempt. The active session is untouched.
    Cancel,
}

#[derive(Debug, Args)]
struct LinkPollArgs {
    /// Keep polling for up to this many seconds; 0 polls exactly once.
    #[arg(long, default_value_t = 0)]
    wait: u64,
}

#[derive(Debug, Args)]
pub(crate) struct BalanceArgs {
    /// Force a fresh balance lookup.
    #[arg(long, default_value_t = false)]
    force: bool,
}

#[derive(Debug, Args)]
pub(crate) struct RewardsCli {
    #[command(subcommand)]
    action: RewardsCommand,
}

#[derive(Debug, Subcommand)]
enum RewardsCommand {
    /// List recent rewards.
    List(LimitArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ChatCli {
    #[command(subcommand)]
    action: ChatCommand,
}

#[derive(Debug, Subcommand)]
enum ChatCommand {
    /// List standard Task Node chat threads.
    #[clap(alias = "conversations")]
    List(LimitArgs),

    /// Read a chat thread.
    History(ChatHistoryArgs),

    /// Search chat threads.
    Search(ChatSearchArgs),

    /// Send a Private Thinking chat message.
    Send(ChatSendArgs),
}

#[derive(Debug, Args)]
struct ChatHistoryArgs {
    conversation_id: String,

    #[arg(long, default_value_t = 120)]
    limit: u16,
}

#[derive(Debug, Args)]
struct ChatSearchArgs {
    query: String,

    #[arg(long, default_value_t = 20)]
    limit: u8,
}

#[derive(Debug, Args)]
struct ChatSendArgs {
    /// Message text. Use --message-file for multiline prompts.
    #[arg(long)]
    message: Option<String>,

    /// Read message text from a file.
    #[arg(long, value_name = "PATH")]
    message_file: Option<PathBuf>,

    /// Existing conversation id. Omit to create a new terminal chat id.
    #[arg(long)]
    conversation_id: Option<String>,

    /// Chat mode. Defaults to Private Thinking.
    #[arg(long, default_value = "Private Thinking")]
    mode: String,

    /// Stream SSE events as JSON lines.
    #[arg(long, default_value_t = false)]
    stream: bool,

    /// Preflight through the backend without calling the model, when the server supports it.
    #[arg(long, default_value_t = false)]
    dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct TeamCli {
    #[command(subcommand)]
    action: TeamCommand,
}

#[derive(Debug, Subcommand)]
enum TeamCommand {
    /// Read the permission-filtered Team Context report.
    Context,
}

#[derive(Debug, Args)]
pub(crate) struct ContextCli {
    #[command(subcommand)]
    action: ContextCommand,
}

#[derive(Debug, Subcommand)]
enum ContextCommand {
    /// Read the current context document.
    Get,

    /// Save a new context document body.
    Save(ContextSaveArgs),
}

#[derive(Debug, Args)]
struct ContextSaveArgs {
    /// Read context body from this file.
    #[arg(long, value_name = "PATH")]
    body_file: PathBuf,

    /// Current revision from `tasknode context get`.
    #[arg(long)]
    revision: u64,

    /// Optional document title.
    #[arg(long)]
    title: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct RequestCli {
    #[command(subcommand)]
    action: RequestCommand,
}

#[derive(Debug, Subcommand)]
enum RequestCommand {
    /// Create a new personal task request.
    Create(RequestCreateArgs),
}

#[derive(Debug, Args)]
struct RequestCreateArgs {
    /// Task request text. Use --body-file for multiline requests.
    #[arg(long)]
    text: Option<String>,

    /// Read task request text from a file.
    #[arg(long, value_name = "PATH")]
    body_file: Option<PathBuf>,

    /// Task request kind.
    #[arg(long, default_value = "personal")]
    kind: String,

    /// Source title recorded in Task Node.
    #[arg(long, default_value = "Corbanu Terminal JSON helper")]
    source_title: String,
}

#[derive(Debug, Args)]
pub(crate) struct RequestsCli {
    #[command(subcommand)]
    action: RequestsCommand,
}

#[derive(Debug, Subcommand)]
enum RequestsCommand {
    /// List active task-generation requests.
    List(RequestsListArgs),

    /// Show one task request.
    Show(RequestShowArgs),

    /// List locally saved submissions, including uncertain network outcomes.
    Pending,

    /// Retry a failed request, fenced to the attempt shown by requests show.
    Retry {
        request_id: String,
        #[arg(long)]
        attempt: u64,
    },

    /// Recover an original submission with its saved key.
    Recover { key: String },
}

#[derive(Debug, Args)]
struct RequestsListArgs {
    #[arg(long, default_value_t = 20)]
    limit: u8,
    #[arg(long)]
    cursor: Option<String>,
}

#[derive(Debug, Args)]
struct RequestShowArgs {
    request_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct TasksCli {
    #[command(subcommand)]
    action: TasksCommand,
}

#[derive(Debug, Subcommand)]
enum TasksCommand {
    /// List tasks in a tab.
    List(TasksListArgs),
}

#[derive(Debug, Args)]
struct TasksListArgs {
    /// Task tab: outstanding, verification, refused, rewarded, etc.
    #[arg(long, default_value = "outstanding")]
    tab: String,
}

#[derive(Debug, Args)]
pub(crate) struct TaskCli {
    #[command(subcommand)]
    action: TaskCommand,
}

#[derive(Debug, Subcommand)]
enum TaskCommand {
    /// Show one task, including terminal-rendered brief text.
    Show(TaskIdArgs),

    /// Accept one task.
    Accept(TaskIdArgs),

    /// Refuse one task.
    Refuse(TaskRefuseArgs),

    /// Cancel one accepted task.
    Cancel(TaskIdArgs),

    /// Submit initial evidence for one accepted task.
    Evidence(TaskEvidenceArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskEvidenceMode {
    InitialSubmission,
    VerificationResponse,
}

impl TaskEvidenceMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::InitialSubmission => "initial_submission",
            Self::VerificationResponse => "verification_response",
        }
    }

    fn command(self, task_id: &str) -> String {
        match self {
            Self::InitialSubmission => {
                format!("corbanu tasknode task evidence {task_id} --body-file <path> --json")
            }
            Self::VerificationResponse => {
                format!("corbanu tasknode verification respond {task_id} --body-file <path> --json")
            }
        }
    }
}

#[derive(Debug, Args)]
struct TaskIdArgs {
    task_id: String,
}

#[derive(Debug, Args)]
struct TaskRefuseArgs {
    task_id: String,

    /// Refusal reason text.
    #[arg(long)]
    reason: Option<String>,

    /// Read refusal reason from a file.
    #[arg(long, value_name = "PATH")]
    reason_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct TaskEvidenceArgs {
    task_id: String,

    /// Evidence summary text.
    #[arg(long)]
    summary: Option<String>,

    /// Read evidence summary from a file.
    #[arg(long, value_name = "PATH")]
    body_file: Option<PathBuf>,

    /// Additional artifact. Accepts a URL or type=value, repeatable.
    #[arg(long = "artifact")]
    artifacts: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct VerificationCli {
    #[command(subcommand)]
    action: VerificationCommand,
}

#[derive(Debug, Subcommand)]
enum VerificationCommand {
    /// Submit a verification response for one task.
    Respond(TaskEvidenceArgs),
}

#[derive(Debug, Args)]
struct LimitArgs {
    #[arg(long, default_value_t = 20)]
    limit: u8,
}

pub(crate) async fn run(command: TaskNodeCli) -> anyhow::Result<()> {
    let result = run_inner(command).await;
    match result {
        Ok(exit_code) => {
            if exit_code != 0 {
                std::process::exit(exit_code);
            }
            Ok(())
        }
        Err(err) => {
            print_json(&json!({
                "ok": false,
                "error": "tasknode_helper_error",
                "message": err.to_string(),
            }))?;
            std::process::exit(1);
        }
    }
}

async fn run_inner(command: TaskNodeCli) -> anyhow::Result<i32> {
    let _json_flag = command.json;
    let inherited_profile = std::env::var("CORBANU_TASKNODE_PROFILE");
    let profile = resolve_tasknode_profile(
        command.config_profile.as_ref(),
        inherited_profile.as_deref(),
        std::env::var_os("CODEX_THREAD_ID").is_some(),
    )?;
    let scope = tasknode_session_scope(profile.as_ref());
    // `link` must work without an existing session; everything else requires one.
    if let TaskNodeCommand::Link(link) = command.command {
        return run_link_command(command.config_overrides, command.origin, scope, link).await;
    }
    let client = TaskNodeClient::from_cli(command.config_overrides, command.origin, &scope).await?;

    match command.command {
        TaskNodeCommand::Link(_) => unreachable!("handled above"),
        TaskNodeCommand::Status => {
            emit_response(client.get("/api/terminal/tasknode/status").await?)
        }
        TaskNodeCommand::Balance(args) => {
            let path = if args.force {
                "/api/terminal/tasknode/balance?force=1"
            } else {
                "/api/terminal/tasknode/balance"
            };
            emit_response(client.get(path).await?)
        }
        TaskNodeCommand::Rewards(cli) => match cli.action {
            RewardsCommand::List(args) => emit_response(
                client
                    .get(&format!(
                        "/api/terminal/tasknode/rewards?limit={}",
                        limit(args.limit, /*min*/ 1, /*max*/ 50)
                    ))
                    .await?,
            ),
        },
        TaskNodeCommand::Chat(cli) => run_chat_command(&client, cli).await,
        TaskNodeCommand::Context(cli) => run_context_command(&client, cli).await,
        TaskNodeCommand::Team(TeamCli {
            action: TeamCommand::Context,
        }) => emit_response(client.get("/api/terminal/tasknode/team/context").await?),
        TaskNodeCommand::Request(cli) => run_request_command(&client, cli).await,
        TaskNodeCommand::Requests(cli) => run_requests_command(&client, cli).await,
        TaskNodeCommand::Tasks(cli) => run_tasks_command(&client, cli).await,
        TaskNodeCommand::Task(cli) => run_task_command(&client, cli).await,
        TaskNodeCommand::Verification(cli) => run_verification_command(&client, cli).await,
    }
}

async fn run_chat_command(client: &TaskNodeClient, cli: ChatCli) -> anyhow::Result<i32> {
    match cli.action {
        ChatCommand::List(args) => emit_response(
            client
                .get(&format!(
                    "/api/terminal/tasknode/chat/conversations?limit={}",
                    limit(args.limit, /*min*/ 1, /*max*/ 50)
                ))
                .await?,
        ),
        ChatCommand::History(args) => emit_response(
            client
                .get(&format!(
                    "/api/terminal/tasknode/chat/history?conversationId={}&limit={}",
                    urlencoding::encode(&args.conversation_id),
                    limit_u16(args.limit, /*min*/ 1, /*max*/ 200)
                ))
                .await?,
        ),
        ChatCommand::Search(args) => emit_response(
            client
                .get(&format!(
                    "/api/terminal/tasknode/chat/search?q={}&limit={}",
                    urlencoding::encode(&args.query),
                    limit(args.limit, /*min*/ 1, /*max*/ 50)
                ))
                .await?,
        ),
        ChatCommand::Send(args) => {
            let message = read_text_input(args.message, args.message_file, "chat message")?;
            let conversation_id = args
                .conversation_id
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(new_chat_id);
            let body = json!({
                "conversationId": conversation_id,
                "message": message,
                "mode": args.mode,
                "dryRun": args.dry_run,
            });
            if args.stream {
                client
                    .post_sse_jsonl("/api/terminal/tasknode/chat/stream", &body)
                    .await
            } else {
                emit_response(
                    client
                        .post("/api/terminal/tasknode/chat/send", &body)
                        .await?,
                )
            }
        }
    }
}

async fn run_context_command(client: &TaskNodeClient, cli: ContextCli) -> anyhow::Result<i32> {
    match cli.action {
        ContextCommand::Get => emit_response(client.get("/api/terminal/tasknode/context").await?),
        ContextCommand::Save(args) => {
            let body_text = read_file_required(&args.body_file, "context body")?;
            let mut body = Map::new();
            body.insert("body".to_string(), Value::String(body_text));
            body.insert("revision".to_string(), Value::from(args.revision));
            body.insert(
                "source".to_string(),
                Value::String("pfterminal-cli".to_string()),
            );
            if let Some(title) = args.title.filter(|value| !value.trim().is_empty()) {
                body.insert("title".to_string(), Value::String(title));
            }
            emit_response(
                client
                    .post("/api/terminal/tasknode/context", &Value::Object(body))
                    .await?,
            )
        }
    }
}

async fn run_request_command(client: &TaskNodeClient, cli: RequestCli) -> anyhow::Result<i32> {
    match cli.action {
        RequestCommand::Create(args) => {
            let detail = read_text_input(args.text, args.body_file, "task request")?;
            let store = client
                .commands
                .as_ref()
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            let command = store
                .begin(&detail, &args.kind)
                .map_err(anyhow::Error::msg)?;
            let mut body = command.body();
            body["sourceConversationTitle"] = Value::String(args.source_title);
            let response = client
                .post("/api/terminal/tasknode/requests", &body)
                .await?;
            if (200..300).contains(&response.status)
                && response.body.get("ok") != Some(&Value::Bool(false))
            {
                store
                    .acknowledge(&command.key, &response.body)
                    .map_err(anyhow::Error::msg)?;
            }
            emit_response(response)
        }
    }
}

async fn run_requests_command(client: &TaskNodeClient, cli: RequestsCli) -> anyhow::Result<i32> {
    match cli.action {
        RequestsCommand::Retry {
            request_id,
            attempt,
        } => emit_response(
            client
                .post(
                    "/api/terminal/tasknode/requests",
                    &client.transport.retry_body(&request_id, attempt),
                )
                .await?,
        ),
        RequestsCommand::Pending => {
            let store = client
                .commands
                .as_ref()
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            print_json(
                &json!({"ok": true, "commands": store.list().map_err(anyhow::Error::msg)?}),
            )?;
            Ok(0)
        }
        RequestsCommand::Recover { key } => {
            let store = client
                .commands
                .as_ref()
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            let command = store
                .list()
                .map_err(anyhow::Error::msg)?
                .into_iter()
                .find(|command| command.key == key)
                .ok_or_else(|| {
                    anyhow::anyhow!("No saved request with that key in this profile and account.")
                })?;
            if let Some(request_id) = &command.request_id {
                return emit_response(
                    client
                        .get(&format!(
                            "/api/terminal/tasknode/requests/{}",
                            urlencoding::encode(request_id)
                        ))
                        .await?,
                );
            }
            let response = client
                .post("/api/terminal/tasknode/requests", &command.body())
                .await?;
            if (200..300).contains(&response.status)
                && response.body.get("ok") != Some(&Value::Bool(false))
            {
                store
                    .acknowledge(&command.key, &response.body)
                    .map_err(anyhow::Error::msg)?;
            }
            emit_response(response)
        }
        RequestsCommand::List(args) => emit_response(
            client
                .get(&format!(
                    "/api/terminal/tasknode/requests?limit={}&cursor={}",
                    limit(args.limit, /*min*/ 1, /*max*/ 50),
                    urlencoding::encode(args.cursor.as_deref().unwrap_or_default())
                ))
                .await?,
        ),
        RequestsCommand::Show(args) => emit_response(
            client
                .get(&format!(
                    "/api/terminal/tasknode/requests/{}",
                    urlencoding::encode(&args.request_id)
                ))
                .await?,
        ),
    }
}

async fn run_tasks_command(client: &TaskNodeClient, cli: TasksCli) -> anyhow::Result<i32> {
    match cli.action {
        TasksCommand::List(args) => emit_response(
            client
                .get(&format!(
                    "/api/terminal/tasknode/tasks?tab={}",
                    urlencoding::encode(&args.tab)
                ))
                .await?,
        ),
    }
}

async fn run_task_command(client: &TaskNodeClient, cli: TaskCli) -> anyhow::Result<i32> {
    match cli.action {
        TaskCommand::Show(args) => emit_response(task_detail(client, &args.task_id).await?),
        TaskCommand::Accept(args) => {
            emit_response(task_action(client, &args.task_id, "accept", /*reason*/ None).await?)
        }
        TaskCommand::Refuse(args) => {
            let reason = read_optional_text_input(args.reason, args.reason_file, "refusal reason")?;
            emit_response(task_action(client, &args.task_id, "refuse", reason).await?)
        }
        TaskCommand::Cancel(args) => {
            emit_response(task_action(client, &args.task_id, "cancel", /*reason*/ None).await?)
        }
        TaskCommand::Evidence(args) => {
            emit_response(task_evidence(client, args, TaskEvidenceMode::InitialSubmission).await?)
        }
    }
}

async fn run_verification_command(
    client: &TaskNodeClient,
    cli: VerificationCli,
) -> anyhow::Result<i32> {
    match cli.action {
        VerificationCommand::Respond(args) => emit_response(
            task_evidence(client, args, TaskEvidenceMode::VerificationResponse).await?,
        ),
    }
}

async fn task_detail(client: &TaskNodeClient, task_id: &str) -> anyhow::Result<TaskNodeResponse> {
    client
        .get(&format!(
            "/api/terminal/tasknode/tasks/{}",
            urlencoding::encode(task_id)
        ))
        .await
}

async fn task_action(
    client: &TaskNodeClient,
    task_id: &str,
    action: &str,
    reason: Option<String>,
) -> anyhow::Result<TaskNodeResponse> {
    let mut body = Map::new();
    body.insert("action".to_string(), Value::String(action.to_string()));
    body.insert(
        "source".to_string(),
        Value::String("pfterminal-cli".to_string()),
    );
    body.insert(
        "idempotencyKey".to_string(),
        Value::String(idempotency_key(action)),
    );
    if let Some(reason) = reason.filter(|value| !value.trim().is_empty()) {
        body.insert("reason".to_string(), Value::String(reason));
    }
    client
        .post(
            &format!(
                "/api/terminal/tasknode/tasks/{}/action",
                urlencoding::encode(task_id)
            ),
            &Value::Object(body),
        )
        .await
}

async fn task_evidence(
    client: &TaskNodeClient,
    args: TaskEvidenceArgs,
    mode: TaskEvidenceMode,
) -> anyhow::Result<TaskNodeResponse> {
    let detail = task_detail(client, &args.task_id).await?;
    if !response_is_ok(&detail) {
        return Ok(detail);
    }
    if let Some(response) = evidence_mode_preflight(&args.task_id, &detail.body, mode) {
        return Ok(response);
    }

    let summary = read_text_input(args.summary, args.body_file, "task evidence")?;
    let body = json!({
        "mode": mode.as_str(),
        "summary": summary,
        "evidence": evidence_items_from_summary_and_artifacts(&summary, &args.artifacts),
        "source": "pfterminal-cli",
        "idempotencyKey": idempotency_key(mode.as_str()),
    });
    let mut response = client
        .post(
            &format!(
                "/api/terminal/tasknode/tasks/{}/evidence",
                urlencoding::encode(&args.task_id)
            ),
            &body,
        )
        .await?;
    if response_is_ok(&response) {
        let refreshed_detail = task_detail(client, &args.task_id).await.ok();
        let Some(submission_ref) = evidence_receipt_reference(&response.body) else {
            return Ok(unconfirmed_evidence_receipt(
                &args.task_id,
                mode,
                response.body,
                refreshed_detail.as_ref(),
            ));
        };
        annotate_evidence_lifecycle(
            &mut response.body,
            &args.task_id,
            mode,
            refreshed_detail.as_ref(),
        );
        if let Some(lifecycle) = response
            .body
            .get_mut("pfterminalLifecycle")
            .and_then(Value::as_object_mut)
        {
            lifecycle.insert("submissionRef".to_string(), submission_ref);
        }
    }
    Ok(response)
}

/// The references Task Node returns for a recorded evidence event: the
/// direct-write path returns all three, the signed-pointer path a txHash.
/// Success without any cannot be confirmed as recorded (#1280).
fn evidence_receipt_reference(receipt: &Value) -> Option<Value> {
    fn text(value: Option<&Value>) -> Option<&str> {
        value
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }
    let mut reference = Map::new();
    for (key, value) in [
        (
            "eventId",
            text(receipt.pointer("/offchainLifecycle/eventId")),
        ),
        ("txHash", text(receipt.get("txHash"))),
        ("cid", text(receipt.get("cid"))),
    ] {
        if let Some(value) = value {
            reference.insert(key.to_string(), Value::String(value.to_string()));
        }
    }
    (!reference.is_empty()).then_some(Value::Object(reference))
}

fn unconfirmed_evidence_receipt(
    task_id: &str,
    mode: TaskEvidenceMode,
    receipt: Value,
    refreshed_detail: Option<&TaskNodeResponse>,
) -> TaskNodeResponse {
    let evidence_gate_open = evidence_gate_open(refreshed_detail, mode);
    let message = match evidence_gate_open {
        Some(true) => {
            "Task Node returned success but no event, transaction or CID reference, and the task still accepts this evidence. Nothing was confirmed as recorded; check the task before submitting again."
        }
        Some(false) | None => {
            "Task Node returned success but no event, transaction or CID reference, so this evidence cannot be confirmed as recorded. Do not resubmit: this evidence step may already be used. Check the task; if it no longer accepts this evidence and shows no further progress, report the task id and this receipt to Task Node operators instead of cancelling the task."
        }
    };
    TaskNodeResponse {
        status: 502,
        body: json!({
            "ok": false,
            "error": "task_evidence_receipt_unconfirmed",
            "message": message,
            "taskId": task_id,
            "submittedMode": mode.as_str(),
            "evidenceGateOpen": evidence_gate_open,
            "nextCommand": format!("corbanu tasknode task show {task_id} --json"),
            "receipt": receipt,
        }),
    }
}

/// The server-reported evidence action for `mode` in a refreshed task detail.
fn evidence_gate_open(
    refreshed_detail: Option<&TaskNodeResponse>,
    mode: TaskEvidenceMode,
) -> Option<bool> {
    let gate = match mode {
        TaskEvidenceMode::InitialSubmission => "canSubmitInitialEvidence",
        TaskEvidenceMode::VerificationResponse => "canSubmitVerificationEvidence",
    };
    refreshed_detail
        .filter(|detail| response_is_ok(detail))
        .and_then(|detail| detail.body.pointer(&format!("/actions/{gate}")))
        .and_then(Value::as_bool)
}

fn evidence_mode_preflight(
    task_id: &str,
    detail: &Value,
    requested_mode: TaskEvidenceMode,
) -> Option<TaskNodeResponse> {
    let actions = detail.get("actions")?;
    let initial_allowed = actions
        .get("canSubmitInitialEvidence")
        .and_then(Value::as_bool);
    let verification_allowed = actions
        .get("canSubmitVerificationEvidence")
        .and_then(Value::as_bool);
    let requested_allowed = match requested_mode {
        TaskEvidenceMode::InitialSubmission => initial_allowed,
        TaskEvidenceMode::VerificationResponse => verification_allowed,
    };
    if requested_allowed != Some(false) {
        return None;
    }

    let alternate_mode = match requested_mode {
        TaskEvidenceMode::InitialSubmission if verification_allowed == Some(true) => {
            Some(TaskEvidenceMode::VerificationResponse)
        }
        TaskEvidenceMode::VerificationResponse if initial_allowed == Some(true) => {
            Some(TaskEvidenceMode::InitialSubmission)
        }
        TaskEvidenceMode::InitialSubmission | TaskEvidenceMode::VerificationResponse => None,
    };
    let (error, message, next_command) = alternate_mode.map_or_else(
        || {
            (
                "task_evidence_not_available",
                "This task does not currently accept evidence. Refresh it and follow the server-reported lifecycle action."
                    .to_string(),
                format!("corbanu tasknode task show {task_id} --json"),
            )
        },
        |mode| {
            (
                "task_evidence_mode_mismatch",
                format!(
                    "This task requires {} evidence, not {} evidence.",
                    mode.as_str(),
                    requested_mode.as_str()
                ),
                mode.command(task_id),
            )
        },
    );
    Some(TaskNodeResponse {
        status: 409,
        body: json!({
            "ok": false,
            "error": error,
            "message": message,
            "taskId": task_id,
            "requestedMode": requested_mode.as_str(),
            "nextCommand": next_command,
        }),
    })
}

fn annotate_evidence_lifecycle(
    response: &mut Value,
    task_id: &str,
    submitted_mode: TaskEvidenceMode,
    refreshed_detail: Option<&TaskNodeResponse>,
) {
    let Some(response) = response.as_object_mut() else {
        return;
    };
    let detail = refreshed_detail
        .filter(|detail| response_is_ok(detail))
        .map(|detail| &detail.body);
    let status = detail
        .and_then(|detail| detail.get("task"))
        .and_then(|task| task.get("statusKey").or_else(|| task.get("status")))
        .and_then(Value::as_str);
    let reward_issued = detail
        .and_then(|detail| detail.get("rewardOutcome"))
        .is_some_and(|outcome| !outcome.is_null())
        || status == Some("rewarded");
    let verification_required = detail
        .and_then(|detail| detail.get("actions"))
        .and_then(|actions| actions.get("canSubmitVerificationEvidence"))
        .and_then(Value::as_bool)
        == Some(true);
    // Task Node reports "no request" as `{}` as well as null.
    let current_verification_request = detail
        .and_then(|detail| detail.get("currentVerificationRequest"))
        .filter(|request| {
            !request.is_null()
                && request
                    .as_object()
                    .is_none_or(|request| !request.is_empty())
        })
        .cloned();

    let (phase, next_command, notice) = if reward_issued {
        (
            "reward_issued",
            "corbanu tasknode rewards list --json".to_string(),
            "Task Node reports a terminal rewarded state.",
        )
    } else if verification_required {
        (
            "verification_required",
            TaskEvidenceMode::VerificationResponse.command(task_id),
            "Initial evidence is not completion. Answer the current verification request before expecting a reward.",
        )
    } else if submitted_mode == TaskEvidenceMode::VerificationResponse {
        (
            "awaiting_reward",
            format!("corbanu tasknode task show {task_id} --json"),
            "The verification response is submitted, but completion is not confirmed until Task Node reports the reward outcome.",
        )
    } else if evidence_gate_open(refreshed_detail, TaskEvidenceMode::InitialSubmission)
        == Some(false)
    {
        (
            "awaiting_verification",
            format!("corbanu tasknode task show {task_id} --json"),
            "Initial evidence is submitted and the initial evidence gate is now closed, so do not resubmit. Completion is not confirmed: Task Node creates the verification request later, so recheck in a later work cycle and answer it. If the task stays submitted with no verification request, report the task id and submission reference to Task Node operators instead of cancelling the task.",
        )
    } else {
        (
            "awaiting_verification",
            format!("corbanu tasknode task show {task_id} --json"),
            "Initial evidence is submitted, but Task Node has not confirmed the new task state yet, so completion and the evidence gate are unconfirmed. Do not resubmit; recheck the task in a later work cycle and answer the verification request when it appears.",
        )
    };

    response.insert(
        "pfterminalLifecycle".to_string(),
        json!({
            "taskId": task_id,
            "submittedMode": submitted_mode.as_str(),
            "phase": phase,
            "completionConfirmed": reward_issued,
            "nextCommand": next_command,
            "notice": notice,
            "currentVerificationRequest": current_verification_request,
        }),
    );
}

#[derive(Clone)]
struct TaskNodeClient {
    transport: codex_tasknode_session::Client,
    commands: Result<codex_tasknode_session::CommandStore, String>,
}

async fn resolve_codex_home(
    config_overrides: CliConfigOverrides,
) -> anyhow::Result<std::path::PathBuf> {
    let cli_kv_overrides = config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;
    let config = ConfigBuilder::default()
        .cli_overrides(cli_kv_overrides)
        .build()
        .await?;
    Ok(config.codex_home.as_path().to_path_buf())
}

async fn run_link_command(
    config_overrides: CliConfigOverrides,
    origin_override: Option<String>,
    scope: codex_tasknode_session::SessionScope,
    link: LinkCli,
) -> anyhow::Result<i32> {
    let codex_home = resolve_codex_home(config_overrides).await?;
    let state = load_tasknode_state(&codex_home, &scope)?;
    let saved_origin = state
        .active
        .as_ref()
        .map(|active| active.origin.clone())
        .or_else(|| state.pending.as_ref().map(|pending| pending.origin.clone()));
    let origin = resolve_origin(origin_override, saved_origin.as_deref());

    match link.action.unwrap_or(LinkCommand::Start) {
        LinkCommand::Start => run_link_start(&codex_home, &scope, &origin, &state).await,
        LinkCommand::Poll(args) => run_link_poll(&codex_home, &scope, &origin, args.wait).await,
        LinkCommand::Status => {
            let mut summary = codex_tasknode_session::state_summary(&state);
            if let Some(map) = summary.as_object_mut() {
                map.insert("ok".to_string(), Value::Bool(true));
                map.insert("origin".to_string(), Value::String(origin));
                map.insert(
                    "profile".to_string(),
                    scope
                        .profile()
                        .map_or(Value::Null, |profile| Value::String(profile.to_string())),
                );
            }
            print_json(&summary)?;
            Ok(0)
        }
        LinkCommand::Cancel => {
            let removed =
                codex_tasknode_session::clear_pending_scoped(&tasknode_vault(&codex_home), &scope)
                    .map_err(|err| anyhow::anyhow!("failed to clear pending link: {err}"))?;
            print_json(&json!({
                "ok": true,
                "action": "link_cancelled",
                "removedPendingAttempt": removed,
                "activeSessionPreserved": state.active.is_some(),
            }))?;
            Ok(0)
        }
    }
}

async fn run_link_start(
    codex_home: &std::path::Path,
    scope: &codex_tasknode_session::SessionScope,
    origin: &str,
    state: &codex_tasknode_session::LocalState,
) -> anyhow::Result<i32> {
    let url = format!("{origin}/api/auth/terminal/start/github");
    let response = normal_http_client()?
        .post(url)
        .json(&json!({}))
        .send()
        .await
        .map_err(reqwest_error)?;
    let response = parse_response(response).await?;
    if !response_is_ok(&response) {
        return emit_response(response);
    }
    let started: codex_tasknode_session::TerminalAuthStart =
        serde_json::from_value(response.body).context("invalid terminal auth start response")?;
    let pending = codex_tasknode_session::PendingLink {
        origin: origin.to_string(),
        request_id: started.request_id.clone(),
        poll_token: started.poll_token.clone(),
        verification_url: started.verification_url.clone(),
        started_at: Some(now_unix_string()),
    };
    codex_tasknode_session::save_pending_scoped(&tasknode_vault(codex_home), scope, &pending)
        .map_err(|err| anyhow::anyhow!("failed to store link attempt: {err}"))?;
    let status_command = scope.profile().map_or_else(
        || "corbanu tasknode status".to_string(),
        |profile| format!("corbanu --profile {profile} tasknode status"),
    );
    print_json(&json!({
        "ok": true,
        "state": "pending",
        "requestId": started.request_id,
        "verificationUrl": started.verification_url,
        "expiresAt": started.expires_at,
        "activeSessionPreserved": state.active.is_some(),
        "nextStep": format!(
            "Open {} in a browser, choose your GitHub account, then run `{status_command}`.",
            started.verification_url
        ),
    }))?;
    Ok(0)
}

async fn run_link_poll(
    codex_home: &std::path::Path,
    scope: &codex_tasknode_session::SessionScope,
    origin: &str,
    wait_seconds: u64,
) -> anyhow::Result<i32> {
    let state = load_tasknode_state(codex_home, scope)?;
    let Some(pending) = state.pending else {
        if state.active.as_ref().is_some_and(|a| !a.is_expired()) {
            print_json(&json!({
                "ok": true,
                "state": "linked",
                "message": "No link attempt is pending; an active session already exists.",
            }))?;
            return Ok(0);
        }
        print_json(&json!({
            "ok": false,
            "error": "tasknode_link_not_started",
            "message": "No link attempt is pending. Run `corbanu tasknode link` first.",
        }))?;
        return Ok(1);
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait_seconds);
    loop {
        let url = format!(
            "{origin}/api/auth/terminal/session?requestId={}&pollToken={}",
            urlencoding::encode(&pending.request_id),
            urlencoding::encode(&pending.poll_token),
        );
        let response = normal_http_client()?
            .get(url)
            .send()
            .await
            .map_err(reqwest_error)?;
        let response = parse_response(response).await?;
        match response.status {
            200 => {
                let issued: codex_tasknode_session::TerminalSessionIssued =
                    serde_json::from_value(response.body.clone())
                        .context("invalid terminal session response")?;
                let candidate =
                    codex_tasknode_session::ActiveSession::from_issued(origin.to_string(), issued);
                // Validate before promoting: the issued token must prove itself
                // against the server before it replaces any stored session.
                let status_url = format!("{origin}/api/terminal/tasknode/status");
                let status_response = normal_http_client()?
                    .get(status_url)
                    .bearer_auth(&candidate.terminal_token)
                    .send()
                    .await
                    .map_err(reqwest_error)?;
                let status_response = parse_response(status_response).await?;
                if !response_is_ok(&status_response) {
                    print_json(&json!({
                        "ok": false,
                        "error": "tasknode_issued_token_invalid",
                        "message": "Issued token failed validation; local state unchanged.",
                        "serverResponse": status_response.body,
                    }))?;
                    return Ok(1);
                }
                codex_tasknode_session::promote_active_scoped(
                    &tasknode_vault(codex_home),
                    scope,
                    &candidate,
                )
                .map_err(|err| anyhow::anyhow!("failed to store session: {err}"))?;
                print_json(&json!({
                    "ok": true,
                    "state": "linked",
                    "accountId": candidate.account_id,
                    "githubUsername": candidate.github_username,
                    "expiresAt": candidate.expires_at,
                }))?;
                return Ok(0);
            }
            202 => {
                if std::time::Instant::now() >= deadline {
                    print_json(&json!({
                        "ok": false,
                        "state": "pending",
                        "verificationUrl": pending.verification_url,
                        "message": "GitHub auth has not completed yet. Finish it in the browser, then poll again.",
                    }))?;
                    return Ok(1);
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            404 | 409 => {
                // The attempt is unknown or consumed server-side; keeping the
                // local record can only produce the same failure forever.
                let _ = codex_tasknode_session::clear_pending_scoped(
                    &tasknode_vault(codex_home),
                    scope,
                );
                print_json(&json!({
                    "ok": false,
                    "error": "tasknode_link_expired",
                    "message": "The link attempt expired or was already used. Run `corbanu tasknode link` to start again.",
                    "serverResponse": response.body,
                }))?;
                return Ok(1);
            }
            _ => return emit_response(response),
        }
    }
}

fn now_unix_string() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs().to_string())
        .unwrap_or_default()
}

impl TaskNodeClient {
    async fn from_cli(
        config_overrides: CliConfigOverrides,
        origin_override: Option<String>,
        scope: &codex_tasknode_session::SessionScope,
    ) -> anyhow::Result<Self> {
        let cli_kv_overrides = config_overrides
            .parse_overrides()
            .map_err(anyhow::Error::msg)?;
        let config = ConfigBuilder::default()
            .cli_overrides(cli_kv_overrides)
            .build()
            .await?;
        let codex_home = config.codex_home.as_path().to_path_buf();
        let session_scope = scope.clone();
        let requested_origin = origin_override
            .clone()
            .or_else(|| std::env::var("PFT_TASKNODE_ORIGIN").ok())
            .or_else(|| std::env::var("TASKNODE_ORIGIN").ok());
        let session = tokio::task::spawn_blocking(move || {
            codex_tasknode_session::resolve_scoped(
                &codex_home,
                &session_scope,
                requested_origin.as_deref(),
            )
        })
        .await?
        .map_err(anyhow::Error::msg)?;
        let origin = resolve_origin(origin_override, Some(session.origin.as_str()));
        Ok(Self {
            transport: codex_tasknode_session::Client::for_session(&session, &origin)?,
            commands: codex_tasknode_session::CommandStore::new(
                config.codex_home.as_path(),
                scope,
                &session,
            ),
        })
    }

    async fn get(&self, path: &str) -> anyhow::Result<TaskNodeResponse> {
        let response = self
            .transport
            .request(reqwest::Method::GET, path, /*body*/ None)
            .await?;
        Ok(TaskNodeResponse {
            status: response.status,
            body: response.body,
        })
    }

    async fn post(&self, path: &str, body: &Value) -> anyhow::Result<TaskNodeResponse> {
        let response = self
            .transport
            .request(reqwest::Method::POST, path, Some(body))
            .await?;
        Ok(TaskNodeResponse {
            status: response.status,
            body: response.body,
        })
    }

    async fn post_sse_jsonl(&self, path: &str, body: &Value) -> anyhow::Result<i32> {
        let mut response = self.transport.stream(path, body).await?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        if !(200..300).contains(&status) || !content_type.contains("text/event-stream") {
            return emit_response(parse_response(response).await?);
        }

        let mut stdout = std::io::stdout();
        let mut decoder = codex_tasknode_session::StreamDecoder::default();
        let mut saw_done = false;
        let mut exit_code = 0;
        while let Some(chunk) = response
            .chunk()
            .await
            .context("failed reading Task Node chat stream")?
        {
            for (event, data) in decoder.push(&chunk).map_err(anyhow::Error::msg)? {
                {
                    if event == "done" {
                        saw_done = true;
                    } else if event == "error" {
                        exit_code = 1;
                    }
                    writeln!(
                        stdout,
                        "{}",
                        serde_json::to_string(&json!({ "event": event, "data": data }))?
                    )?;
                    stdout.flush()?;
                }
            }
        }
        decoder.finish().map_err(anyhow::Error::msg)?;
        stdout.flush()?;
        if !saw_done && exit_code == 0 {
            print_json(&json!({
                "ok": false,
                "error": "tasknode_stream_incomplete",
                "message": "Task Node chat stream ended without a done event.",
            }))?;
            return Ok(1);
        }
        Ok(exit_code)
    }
}

#[derive(Debug)]
struct TaskNodeResponse {
    status: u16,
    body: Value,
}

fn tasknode_vault(codex_home: &std::path::Path) -> codex_vault::Vault {
    codex_vault::Vault::new(codex_home.to_path_buf())
}

fn load_tasknode_state(
    codex_home: &std::path::Path,
    scope: &codex_tasknode_session::SessionScope,
) -> anyhow::Result<codex_tasknode_session::LocalState> {
    codex_tasknode_session::load_scoped(&tasknode_vault(codex_home), scope)
        .map_err(|err| anyhow::anyhow!("failed to read local Task Node state: {err}"))
}

fn resolve_tasknode_profile(
    explicit: Option<&ProfileV2Name>,
    inherited: Result<&str, &std::env::VarError>,
    agent_session: bool,
) -> anyhow::Result<Option<ProfileV2Name>> {
    match inherited {
        Ok(value) => {
            let profile: Option<String> = serde_json::from_str(value).context(
                "Invalid inherited Task Node profile; refusing default-account fallback",
            )?;
            let profile = profile
                .map(|name| name.parse::<ProfileV2Name>())
                .transpose()
                .context(
                    "Invalid inherited Task Node profile; refusing default-account fallback",
                )?;
            if explicit.is_some() && explicit != profile.as_ref() {
                anyhow::bail!(
                    "Task Node profile conflicts with the active terminal profile; switch profiles in Corbanu first"
                );
            }
            Ok(profile)
        }
        Err(std::env::VarError::NotPresent) => {
            if agent_session {
                anyhow::bail!(
                    "Active Task Node profile was not supplied by this terminal. Restart the updated terminal to inherit its profile; refusing default-account fallback"
                );
            }
            Ok(explicit.cloned())
        }
        Err(_) => {
            anyhow::bail!("Invalid inherited Task Node profile; refusing default-account fallback")
        }
    }
}

fn tasknode_session_scope(profile: Option<&ProfileV2Name>) -> codex_tasknode_session::SessionScope {
    profile
        .map(|profile| codex_tasknode_session::SessionScope::for_profile(profile.as_str()))
        .unwrap_or_default()
}

fn resolve_origin(origin_override: Option<String>, saved_origin: Option<&str>) -> String {
    origin_override
        .or_else(|| std::env::var("PFT_TASKNODE_ORIGIN").ok())
        .or_else(|| std::env::var("TASKNODE_ORIGIN").ok())
        .or_else(|| saved_origin.map(ToString::to_string))
        .unwrap_or_else(|| DEFAULT_TASKNODE_ORIGIN.to_string())
        .trim_end_matches('/')
        .to_string()
}

fn normal_http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(45))
        .build()
        .map_err(reqwest_error)
}

async fn parse_response(response: reqwest::Response) -> anyhow::Result<TaskNodeResponse> {
    let status = response.status().as_u16();
    let text = response.text().await.map_err(reqwest_error)?;
    let body = serde_json::from_str::<Value>(&text).unwrap_or_else(|_| {
        json!({
            "ok": false,
            "error": "tasknode_non_json_response",
            "message": text,
            "httpStatus": status,
        })
    });
    Ok(TaskNodeResponse { status, body })
}

fn emit_response(mut response: TaskNodeResponse) -> anyhow::Result<i32> {
    if response.status == 401 {
        let message = codex_tasknode_session::Response {
            status: response.status,
            body: response.body.clone(),
        }
        .message();
        if let Some(body) = response.body.as_object_mut() {
            body.insert("message".into(), Value::String(message));
        }
    }
    print_json(&response.body)?;
    Ok(if response_is_ok(&response) { 0 } else { 1 })
}

fn response_is_ok(response: &TaskNodeResponse) -> bool {
    (200..300).contains(&response.status)
        && response.body.get("ok").and_then(Value::as_bool) != Some(false)
}

fn print_json(value: &Value) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout();
    writeln!(stdout, "{}", serde_json::to_string(value)?)?;
    stdout.flush()?;
    Ok(())
}

fn read_text_input(
    inline: Option<String>,
    file: Option<PathBuf>,
    label: &str,
) -> anyhow::Result<String> {
    match (inline, file) {
        (Some(_), Some(_)) => anyhow::bail!(
            "Provide either --message/--text/--summary or a file for {label}, not both."
        ),
        (Some(text), None) => require_nonempty_text(text, label),
        (None, Some(path)) => read_file_required(&path, label),
        (None, None) => anyhow::bail!("{label} is required."),
    }
}

fn read_optional_text_input(
    inline: Option<String>,
    file: Option<PathBuf>,
    label: &str,
) -> anyhow::Result<Option<String>> {
    match (inline, file) {
        (Some(_), Some(_)) => {
            anyhow::bail!("Provide either inline text or a file for {label}, not both.")
        }
        (Some(text), None) => Ok(Some(require_nonempty_text(text, label)?)),
        (None, Some(path)) => Ok(Some(read_file_required(&path, label)?)),
        (None, None) => Ok(None),
    }
}

fn read_file_required(path: &PathBuf, label: &str) -> anyhow::Result<String> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed reading {label} file {}", path.display()))?;
    require_nonempty_text(text, label)
}

fn require_nonempty_text(text: String, label: &str) -> anyhow::Result<String> {
    if text.trim().is_empty() {
        anyhow::bail!("{label} is empty.");
    }
    Ok(text)
}

fn idempotency_key(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("pfterminal-cli:{prefix}:{}:{nanos}", std::process::id())
}

fn new_chat_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("chat_cli_{}_{}", std::process::id(), nanos)
}

fn limit(value: u8, min: u8, max: u8) -> u8 {
    value.clamp(min, max)
}

fn limit_u16(value: u16, min: u16, max: u16) -> u16 {
    value.clamp(min, max)
}

fn evidence_items_from_summary_and_artifacts(summary: &str, artifacts: &[String]) -> Vec<Value> {
    let mut items = artifacts
        .iter()
        .filter_map(|artifact| evidence_item_from_artifact(artifact))
        .collect::<Vec<_>>();
    for url in summary
        .split_whitespace()
        .filter(|part| is_http_url(part))
        .take(5)
    {
        if !items
            .iter()
            .any(|item| evidence_item_value(item) == Some(url))
        {
            items.push(evidence_item_from_value(infer_artifact_type(url), url));
        }
    }
    items
}

fn evidence_item_from_artifact(artifact: &str) -> Option<Value> {
    let trimmed = artifact.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Query and path parameters are part of a bare URL, not a type delimiter.
    if !is_http_url(trimmed)
        && let Some((kind, value)) = trimmed.split_once('=')
    {
        let value = value.trim();
        if value.is_empty() {
            return None;
        }
        return Some(evidence_item_from_value(kind.trim(), value));
    }
    Some(evidence_item_from_value(
        infer_artifact_type(trimmed),
        trimmed,
    ))
}

fn evidence_item_from_value(kind: &str, value: &str) -> Value {
    if is_http_url(value) {
        json!({ "type": kind, "url": value })
    } else {
        json!({ "type": kind, "value": value })
    }
}

fn evidence_item_value(item: &Value) -> Option<&str> {
    item.get("url")
        .or_else(|| item.get("value"))
        .or_else(|| item.get("text"))
        .and_then(Value::as_str)
}

fn infer_artifact_type(value: &str) -> &'static str {
    if !is_http_url(value) {
        return "text";
    }
    if let Ok(url) = url::Url::parse(value)
        && matches!(url.host_str(), Some("github.com" | "www.github.com"))
        && let Some(mut segments) = url.path_segments()
        && segments.next().is_some_and(|owner| !owner.is_empty())
        && segments.next().is_some_and(|repo| !repo.is_empty())
    {
        let kind = segments.next();
        if segments.next().is_some_and(|resource| !resource.is_empty()) {
            match kind {
                Some("pull") => return "github_pr",
                Some("commit") => return "git_commit",
                _ => {}
            }
        }
    }
    "url"
}

fn is_http_url(value: &str) -> bool {
    value.split_once("://").is_some_and(|(scheme, _)| {
        scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
    })
}

#[cfg(test)]
fn tasknode_sse_separator(buffer: &str) -> Option<(usize, usize)> {
    match (buffer.find("\n\n"), buffer.find("\r\n\r\n")) {
        (Some(lf), Some(crlf)) if crlf < lf => Some((crlf, 4)),
        (Some(lf), _) => Some((lf, 2)),
        (None, Some(crlf)) => Some((crlf, 4)),
        (None, None) => None,
    }
}

#[cfg(test)]
fn tasknode_sse_drain_blocks(buffer: &mut String) -> Vec<String> {
    let mut blocks = Vec::new();
    while let Some((index, separator_len)) = tasknode_sse_separator(buffer) {
        let drained: String = buffer.drain(..index + separator_len).collect();
        blocks.push(drained[..index].to_string());
    }
    blocks
}

#[cfg(test)]
fn tasknode_parse_sse_block(block: &str) -> anyhow::Result<Option<(String, Value)>> {
    let normalized = block.replace("\r\n", "\n");
    let mut event = "message".to_string();
    let mut data = Vec::new();
    for line in normalized.lines() {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            event = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("data:") {
            data.push(rest.trim_start().to_string());
        }
    }
    if data.is_empty() {
        return Ok(None);
    }
    let data = data.join("\n");
    if data.trim() == "[DONE]" {
        return Ok(None);
    }
    let value = serde_json::from_str(&data)
        .with_context(|| format!("invalid Task Node chat stream event: {data}"))?;
    Ok(Some((event, value)))
}

fn reqwest_error(err: reqwest::Error) -> anyhow::Error {
    let mut message = err.to_string();
    let mut source = std::error::Error::source(&err);
    while let Some(err) = source {
        let part = err.to_string();
        if !part.is_empty() && !message.contains(&part) {
            message.push_str(": ");
            message.push_str(&part);
        }
        source = std::error::Error::source(err);
    }
    anyhow::anyhow!(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn team_context_is_read_only_and_accepts_profile_scope() {
        let cli = TaskNodeCli::try_parse_from([
            "tasknode",
            "--profile",
            "alice",
            "team",
            "context",
            "--json",
        ])
        .unwrap();
        assert_eq!(cli.config_profile.as_deref(), Some("alice"));
        assert!(matches!(
            cli.command,
            TaskNodeCommand::Team(TeamCli {
                action: TeamCommand::Context
            })
        ));
        assert!(
            TaskNodeCli::try_parse_from(["tasknode", "team", "context", "--account", "bob"])
                .is_err()
        );
        assert!(TaskNodeCli::try_parse_from(["tasknode", "team", "save"]).is_err());
    }

    #[test]
    fn tasknode_cli_accepts_profile_scoped_credentials() {
        let cli = TaskNodeCli::try_parse_from([
            "tasknode",
            "--profile",
            "goodalexander",
            "link",
            "status",
        ])
        .expect("parse profile-scoped Task Node command");

        assert_eq!(
            cli.config_profile.as_deref(),
            Some("goodalexander"),
            "the helper must select the same credential scope as the profile tab"
        );
        assert_eq!(
            tasknode_session_scope(cli.config_profile.as_ref()).profile(),
            Some("goodalexander")
        );
    }

    #[test]
    fn tasknode_agent_profile_resolution_fails_closed() {
        let alice: ProfileV2Name = "alice".parse().unwrap();
        let bob: ProfileV2Name = "bob".parse().unwrap();
        let missing = std::env::VarError::NotPresent;
        for (explicit, inherited) in [
            (None, Err(&missing)),
            (Some(&alice), Err(&missing)),
            (None, Ok("alice")),
            (None, Ok("42")),
            (None, Ok("\"../alice\"")),
            (Some(&bob), Ok("\"alice\"")),
            (Some(&alice), Ok("null")),
        ] {
            assert!(resolve_tasknode_profile(explicit, inherited, /*agent_session*/ true).is_err());
        }
        for (explicit, inherited, expected) in [
            (None, "\"alice\"", Some(alice.clone())),
            (Some(&alice), "\"alice\"", Some(alice.clone())),
            (None, "\"bob\"", Some(bob.clone())),
            (None, "null", None),
        ] {
            assert_eq!(
                resolve_tasknode_profile(explicit, Ok(inherited), /*agent_session*/ true).unwrap(),
                expected
            );
        }
        assert_eq!(
            resolve_tasknode_profile(
                /*explicit*/ None,
                Err(&missing),
                /*agent_session*/ false
            )
            .unwrap(),
            None
        );
        assert_eq!(
            resolve_tasknode_profile(Some(&bob), Err(&missing), /*agent_session*/ false).unwrap(),
            Some(bob)
        );
    }

    #[test]
    fn tasknode_inherited_profiles_load_distinct_linked_accounts() {
        let home = tempfile::tempdir().unwrap();
        let vault = tasknode_vault(home.path());
        for (profile, account) in [
            (None, "default-account"),
            (Some("alice"), "alice-account"),
            (Some("bob"), "bob-account"),
        ] {
            let profile = profile.map(|name| name.parse::<ProfileV2Name>().unwrap());
            codex_tasknode_session::promote_active_scoped(
                &vault,
                &tasknode_session_scope(profile.as_ref()),
                &codex_tasknode_session::ActiveSession {
                    origin: "http://127.0.0.1:1".to_string(),
                    account_id: Some(account.to_string()),
                    github_username: Some(account.to_string()),
                    terminal_token: format!("fixture-{account}"),
                    expires_at: None,
                },
            )
            .unwrap();
        }
        for (inherited, account) in [
            ("null", "default-account"),
            ("\"alice\"", "alice-account"),
            ("\"bob\"", "bob-account"),
        ] {
            let profile = resolve_tasknode_profile(
                /*explicit*/ None,
                Ok(inherited),
                /*agent_session*/ true,
            )
            .unwrap();
            let active = codex_tasknode_session::load_scoped(
                &vault,
                &tasknode_session_scope(profile.as_ref()),
            )
            .unwrap()
            .active
            .unwrap();
            assert_eq!(
                (
                    active.account_id,
                    active.github_username,
                    active.terminal_token
                ),
                (
                    Some(account.to_string()),
                    Some(account.to_string()),
                    format!("fixture-{account}")
                )
            );
        }
    }

    #[test]
    fn evidence_mode_preflight_routes_verification_to_the_distinct_command() {
        let detail = json!({
            "actions": {
                "canSubmitInitialEvidence": false,
                "canSubmitVerificationEvidence": true
            }
        });

        let response =
            evidence_mode_preflight("task_test", &detail, TaskEvidenceMode::InitialSubmission)
                .expect("mode mismatch");

        assert_eq!(
            response.body,
            json!({
                "ok": false,
                "error": "task_evidence_mode_mismatch",
                "message": "This task requires verification_response evidence, not initial_submission evidence.",
                "taskId": "task_test",
                "requestedMode": "initial_submission",
                "nextCommand": "corbanu tasknode verification respond task_test --body-file <path> --json"
            })
        );
    }

    #[test]
    fn initial_evidence_receipt_exposes_unfinished_verification_stage() {
        let mut response = json!({ "ok": true, "receiptId": "receipt_test" });
        let detail = TaskNodeResponse {
            status: 200,
            body: json!({
                "task": { "statusKey": "verification_requested" },
                "actions": { "canSubmitVerificationEvidence": true },
                "currentVerificationRequest": {
                    "body": "Provide the exact test result."
                }
            }),
        };

        annotate_evidence_lifecycle(
            &mut response,
            "task_test",
            TaskEvidenceMode::InitialSubmission,
            Some(&detail),
        );

        assert_eq!(
            response,
            json!({
                "ok": true,
                "receiptId": "receipt_test",
                "pfterminalLifecycle": {
                    "taskId": "task_test",
                    "submittedMode": "initial_submission",
                    "phase": "verification_required",
                    "completionConfirmed": false,
                    "nextCommand": "corbanu tasknode verification respond task_test --body-file <path> --json",
                    "notice": "Initial evidence is not completion. Answer the current verification request before expecting a reward.",
                    "currentVerificationRequest": {
                        "body": "Provide the exact test result."
                    }
                }
            })
        );
    }

    #[test]
    fn verification_receipt_only_confirms_completion_with_reward_outcome() {
        let mut response = json!({ "ok": true });
        let detail = TaskNodeResponse {
            status: 200,
            body: json!({
                "task": { "statusKey": "rewarded" },
                "actions": {},
                "rewardOutcome": { "rewardPft": 25 }
            }),
        };

        annotate_evidence_lifecycle(
            &mut response,
            "task_rewarded",
            TaskEvidenceMode::VerificationResponse,
            Some(&detail),
        );

        assert_eq!(
            response["pfterminalLifecycle"],
            json!({
                "taskId": "task_rewarded",
                "submittedMode": "verification_response",
                "phase": "reward_issued",
                "completionConfirmed": true,
                "nextCommand": "corbanu tasknode rewards list --json",
                "notice": "Task Node reports a terminal rewarded state.",
                "currentVerificationRequest": null
            })
        );
    }

    #[test]
    fn infers_evidence_items_from_summary_urls_and_artifacts() {
        let items = evidence_items_from_summary_and_artifacts(
            "Implemented in https://github.com/postfiatorg/tasknodeofficial/pull/192 and commit https://github.com/postfiatorg/tasknodeofficial/commit/abc",
            &["log=terminal smoke passed".to_string()],
        );

        assert_eq!(items.len(), 3);
        assert_eq!(items[0].get("type").and_then(Value::as_str), Some("log"));
        assert_eq!(
            items[1].get("type").and_then(Value::as_str),
            Some("github_pr")
        );
        assert_eq!(
            items[2].get("type").and_then(Value::as_str),
            Some("git_commit")
        );
    }

    #[test]
    fn bare_artifact_urls_preserve_equals_signs() {
        for (url, kind) in [
            ("https://example.test/report?a=1&b=two", "url"),
            ("HTTPS://example.test/report?a=1&b=two", "url"),
            ("HtTp://example.test/report?a=1&b=two", "url"),
            (
                "http://example.test/a;v=1?q=a%3Db&next=%2Fpart#section-2",
                "url",
            ),
            (
                "https://github.com/owner/repo/pull/42?diff=split",
                "github_pr",
            ),
            (
                "HTTPS://github.com/owner/repo/pull/42?diff=split",
                "github_pr",
            ),
        ] {
            assert_eq!(
                evidence_items_from_summary_and_artifacts("Reviewed", &[url.to_string()]),
                vec![json!({"type": kind, "url": url})],
            );
        }
        assert_eq!(
            evidence_items_from_summary_and_artifacts(
                "Reviewed",
                &["url=https://example.test/report?a=1&b=two".to_string()],
            ),
            vec![json!({"type": "url", "url": "https://example.test/report?a=1&b=two"})],
        );
    }

    #[test]
    fn artifact_type_uses_the_actual_github_host_and_resource_path() {
        for value in [
            "https://notgithub.com/owner/repo/pull/42",
            "https://example.test/?next=https://github.com/owner/repo/commit/abc",
            "https://github.com/owner/repo/issues/1?next=/pull/42",
            "https://github.com/owner/repo/wiki/pull/42",
        ] {
            let expected = vec![json!({"type": "url", "url": value})];
            assert_eq!(
                evidence_items_from_summary_and_artifacts("Reviewed", &[value.to_string()]),
                expected,
            );
            assert_eq!(
                evidence_items_from_summary_and_artifacts(value, &[]),
                expected
            );
        }
        assert_eq!(
            infer_artifact_type("Note mentioning github.com/owner/repo/pull/42"),
            "text",
        );
        assert_eq!(
            infer_artifact_type("https://github.com/owner/repo/pull/42/files"),
            "github_pr",
        );
        assert_eq!(
            infer_artifact_type("https://github.com/owner/repo/commit/abc#diff-1"),
            "git_commit",
        );
    }

    #[test]
    fn parses_sse_delta_and_done_blocks() {
        let mut buffer = String::new();
        buffer.push_str("event: delta\ndata: {\"delta\":\"hi\"}\n\n");
        buffer.push_str("event: done\ndata: {\"ok\":true}\n\n");

        let blocks = tasknode_sse_drain_blocks(&mut buffer);
        assert_eq!(blocks.len(), 2);

        let first = tasknode_parse_sse_block(&blocks[0])
            .expect("valid first block")
            .expect("first event");
        assert_eq!(first.0, "delta");
        assert_eq!(first.1.get("delta").and_then(Value::as_str), Some("hi"));

        let second = tasknode_parse_sse_block(&blocks[1])
            .expect("valid second block")
            .expect("second event");
        assert_eq!(second.0, "done");
        assert_eq!(second.1.get("ok").and_then(Value::as_bool), Some(true));
    }

    #[test]
    fn parses_crlf_sse_blocks() {
        let mut buffer = "event: error\r\ndata: {\"message\":\"failed\"}\r\n\r\n".to_string();
        let blocks = tasknode_sse_drain_blocks(&mut buffer);
        assert_eq!(blocks.len(), 1);
        let parsed = tasknode_parse_sse_block(&blocks[0])
            .expect("valid crlf block")
            .expect("event");
        assert_eq!(parsed.0, "error");
        assert_eq!(
            parsed.1.get("message").and_then(Value::as_str),
            Some("failed")
        );
    }

    /// Stub Task Node: the pre-submit detail opens the initial gate, the POST
    /// returns `receipt`, and the post-submit detail returns `after`.
    async fn submit_initial_evidence_against_stub(
        receipt: Value,
        after: Value,
    ) -> TaskNodeResponse {
        use wiremock::Mock;
        use wiremock::MockServer;
        use wiremock::ResponseTemplate;
        use wiremock::matchers::method;
        use wiremock::matchers::path;

        let server = MockServer::start().await;
        let detail_path = "/api/terminal/tasknode/tasks/task_stub";
        Mock::given(method("GET"))
            .and(path(detail_path))
            .respond_with(ResponseTemplate::new(200).set_body_json(accepted_detail()))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(detail_path))
            .respond_with(ResponseTemplate::new(200).set_body_json(after))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("{detail_path}/evidence")))
            .respond_with(ResponseTemplate::new(200).set_body_json(receipt))
            .expect(1)
            .mount(&server)
            .await;

        let client = TaskNodeClient {
            transport: codex_tasknode_session::Client::anonymous(&server.uri()).unwrap(),
            commands: Err("unused".to_string()),
        };
        let args = TaskEvidenceArgs {
            task_id: "task_stub".to_string(),
            summary: Some("Five passing runs: https://gist.github.com/example/abc".to_string()),
            body_file: None,
            artifacts: Vec::new(),
        };
        task_evidence(&client, args, TaskEvidenceMode::InitialSubmission)
            .await
            .unwrap()
    }

    fn accepted_detail() -> Value {
        json!({
            "ok": true,
            "task": { "statusKey": "accepted" },
            "actions": {
                "canSubmitInitialEvidence": true,
                "canSubmitVerificationEvidence": false
            }
        })
    }

    /// The task view reported in #1280 after a direct-write submission: the
    /// gate is closed, with no summaries and no verification request (`{}`).
    fn submitted_without_review_detail() -> Value {
        json!({
            "ok": true,
            "task": { "statusKey": "submitted" },
            "actions": {
                "canSubmitInitialEvidence": false,
                "canSubmitVerificationEvidence": false
            },
            "submission": { "summaries": [] },
            "currentVerificationRequest": {},
            "rewardOutcome": null,
            "forensics": { "source": "direct_write" }
        })
    }

    // The reporter's real receipt carried references (see the next test);
    // this pairs the #1280 task view with a receipt that confirms nothing.
    #[tokio::test]
    async fn evidence_success_without_a_recorded_reference_fails_loudly() {
        let response = submit_initial_evidence_against_stub(
            json!({ "ok": true, "phase": "submitted" }),
            submitted_without_review_detail(),
        )
        .await;

        assert_eq!(response.status, 502);
        assert_eq!(
            response.body,
            json!({
                "ok": false,
                "error": "task_evidence_receipt_unconfirmed",
                "message": "Task Node returned success but no event, transaction or CID reference, so this evidence cannot be confirmed as recorded. Do not resubmit: this evidence step may already be used. Check the task; if it no longer accepts this evidence and shows no further progress, report the task id and this receipt to Task Node operators instead of cancelling the task.",
                "taskId": "task_stub",
                "submittedMode": "initial_submission",
                "evidenceGateOpen": false,
                "nextCommand": "corbanu tasknode task show task_stub --json",
                "receipt": { "ok": true, "phase": "submitted" }
            })
        );
    }

    #[tokio::test]
    async fn unconfirmed_receipt_says_when_the_gate_is_still_open() {
        let response = submit_initial_evidence_against_stub(
            json!({ "ok": true, "cid": " ", "txHash": "" }),
            accepted_detail(),
        )
        .await;

        assert_eq!(response.status, 502);
        assert_eq!(
            response.body,
            json!({
                "ok": false,
                "error": "task_evidence_receipt_unconfirmed",
                "message": "Task Node returned success but no event, transaction or CID reference, and the task still accepts this evidence. Nothing was confirmed as recorded; check the task before submitting again.",
                "taskId": "task_stub",
                "submittedMode": "initial_submission",
                "evidenceGateOpen": true,
                "nextCommand": "corbanu tasknode task show task_stub --json",
                "receipt": { "ok": true, "cid": " ", "txHash": "" }
            })
        );
    }

    #[tokio::test]
    async fn direct_write_receipt_reports_event_and_unanswered_review_honestly() {
        let response = submit_initial_evidence_against_stub(
            json!({
                "ok": true,
                "phase": "submitted",
                "message": "Task evidence recorded directly in Task Node.",
                "taskId": "task_stub",
                "cid": "postgres:task_evt_stub",
                "txHash": "offchain:task_evt_stub",
                "offchainLifecycle": {
                    "writeSource": "direct_write",
                    "eventId": "task_evt_stub",
                    "transition": "submitted"
                }
            }),
            submitted_without_review_detail(),
        )
        .await;

        assert!(response_is_ok(&response), "{}", response.body);
        assert_eq!(
            response.body["pfterminalLifecycle"],
            json!({
                "taskId": "task_stub",
                "submittedMode": "initial_submission",
                "phase": "awaiting_verification",
                "completionConfirmed": false,
                "nextCommand": "corbanu tasknode task show task_stub --json",
                "notice": "Initial evidence is submitted and the initial evidence gate is now closed, so do not resubmit. Completion is not confirmed: Task Node creates the verification request later, so recheck in a later work cycle and answer it. If the task stays submitted with no verification request, report the task id and submission reference to Task Node operators instead of cancelling the task.",
                "currentVerificationRequest": null,
                "submissionRef": {
                    "eventId": "task_evt_stub",
                    "txHash": "offchain:task_evt_stub",
                    "cid": "postgres:task_evt_stub"
                }
            })
        );
    }

    /// The signed-pointer path returns only a txHash, and its projection
    /// refresh is asynchronous, so the task may still look accepted.
    #[tokio::test]
    async fn signed_pointer_receipt_is_confirmed_by_its_tx_hash() {
        let response = submit_initial_evidence_against_stub(
            json!({
                "ok": true,
                "phase": "submitted",
                "message": "Task evidence published to PFT.",
                "taskId": "task_stub",
                "cid": "",
                "txHash": "ABC123",
                "engineResult": "tesSUCCESS"
            }),
            accepted_detail(),
        )
        .await;

        assert!(response_is_ok(&response), "{}", response.body);
        assert_eq!(
            response.body["pfterminalLifecycle"],
            json!({
                "taskId": "task_stub",
                "submittedMode": "initial_submission",
                "phase": "awaiting_verification",
                "completionConfirmed": false,
                "nextCommand": "corbanu tasknode task show task_stub --json",
                "notice": "Initial evidence is submitted, but Task Node has not confirmed the new task state yet, so completion and the evidence gate are unconfirmed. Do not resubmit; recheck the task in a later work cycle and answer the verification request when it appears.",
                "currentVerificationRequest": null,
                "submissionRef": { "txHash": "ABC123" }
            })
        );
    }
}
