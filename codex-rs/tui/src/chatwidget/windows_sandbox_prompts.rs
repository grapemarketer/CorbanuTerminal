//! Windows sandbox prompts and warning surfaces for `ChatWidget`.

use super::*;

/// Names the non-admin sandbox's main gaps next to the option that selects it.
#[cfg(any(target_os = "windows", test))]
const LEGACY_SANDBOX_OPTION_DESCRIPTION: &str =
    "Doesn't block reading your files; its network block can be bypassed";

impl ChatWidget {
    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn windows_sandbox_mode_allowed(&self, mode: WindowsSandboxModeToml) -> bool {
        self.config
            .config_layer_stack
            .requirements()
            .windows_sandbox_mode
            .can_set(&Some(mode))
            .is_ok()
    }

    #[cfg(any(target_os = "windows", test))]
    pub(super) fn elevated_windows_sandbox_setup_required(&self) -> bool {
        crate::windows_sandbox::level_from_config(&self.config) == WindowsSandboxLevel::Elevated
            && self
                .config
                .config_layer_stack
                .requirements()
                .windows_sandbox_mode
                .source
                .is_some()
            && !crate::windows_sandbox::sandbox_setup_is_complete(self.config.codex_home.as_path())
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn world_writable_warning_details(&self) -> Option<(Vec<String>, usize, bool)> {
        if self
            .config
            .notices
            .hide_world_writable_warning
            .unwrap_or(false)
        {
            return None;
        }
        let cwd = self.config.cwd.clone();
        let workspace_roots = self.config.effective_workspace_roots();
        let env_map: std::collections::HashMap<String, String> = std::env::vars().collect();
        let permission_profile = self.config.permissions.effective_permission_profile();
        let Ok(permissions) =
            codex_windows_sandbox::ResolvedWindowsSandboxPermissions::try_from_permission_profile_for_workspace_roots(
                &permission_profile,
                workspace_roots.as_slice(),
            )
        else {
            return None;
        };
        match codex_windows_sandbox::apply_world_writable_scan_and_denies_for_permissions(
            self.config.codex_home.as_path(),
            cwd.as_path(),
            &env_map,
            &permissions,
            Some(self.config.codex_home.as_path()),
        ) {
            Ok(_) => None,
            Err(_) => Some((Vec::new(), 0, true)),
        }
    }

    #[cfg(not(target_os = "windows"))]
    #[allow(dead_code)]
    pub(crate) fn world_writable_warning_details(&self) -> Option<(Vec<String>, usize, bool)> {
        None
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn open_world_writable_warning_confirmation(
        &mut self,
        preset: Option<ApprovalPreset>,
        profile_selection: Option<PermissionProfileSelection>,
        sample_paths: Vec<String>,
        extra_count: usize,
        failed_scan: bool,
    ) {
        let (approval, permission_profile, active_permission_profile) = match &preset {
            Some(p) => (
                Some(AskForApproval::from(p.approval)),
                Some(p.permission_profile.clone()),
                Some(p.active_permission_profile.clone()),
            ),
            None => (None, None, None),
        };
        let mut header_children: Vec<Box<dyn Renderable>> = Vec::new();
        let describe_profile = |profile: &PermissionProfile| {
            if matches!(profile, PermissionProfile::Disabled) {
                "Full Access mode"
            } else if profile
                .file_system_sandbox_policy()
                .can_write_path_with_cwd(self.config.cwd.as_path(), self.config.cwd.as_path())
            {
                "Agent mode"
            } else {
                "Read-Only mode"
            }
        };
        let mode_label = preset
            .as_ref()
            .map(|p| describe_profile(&p.permission_profile))
            .unwrap_or_else(|| {
                describe_profile(&self.config.permissions.effective_permission_profile())
            });
        let info_line = if failed_scan {
            Line::from(vec![
                "We couldn't complete the world-writable scan, so protections cannot be verified. "
                    .into(),
                format!("The Windows sandbox cannot guarantee protection in {mode_label}.")
                    .fg(Color::Red),
            ])
        } else {
            Line::from(vec![
                "The Windows sandbox cannot protect writes to folders that are writable by Everyone.".into(),
                " Consider removing write access for Everyone from the following folders:".into(),
            ])
        };
        header_children.push(Box::new(
            Paragraph::new(vec![info_line]).wrap(Wrap { trim: false }),
        ));

        if !sample_paths.is_empty() {
            // Show up to three examples and optionally an "and X more" line.
            let mut lines: Vec<Line> = Vec::new();
            lines.push(Line::from(""));
            for p in &sample_paths {
                lines.push(Line::from(format!("  - {p}")));
            }
            if extra_count > 0 {
                lines.push(Line::from(format!("and {extra_count} more")));
            }
            header_children.push(Box::new(Paragraph::new(lines).wrap(Wrap { trim: false })));
        }
        let header = ColumnRenderable::with(header_children);

        // Build actions ensuring acknowledgement happens before applying the
        // new permission profile, so downstream policy-change hooks don't
        // re-trigger the warning.
        let mut accept_actions: Vec<SelectionAction> = Vec::new();
        // Suppress the immediate re-scan only when a preset will be applied via
        // /permissions, to avoid duplicate warnings from the ensuing policy change.
        if preset.is_some() {
            accept_actions.push(Box::new(|tx| {
                tx.send(AppEvent::SkipNextWorldWritableScan);
            }));
        }
        if let Some(selection) = profile_selection.clone() {
            accept_actions.extend(Self::permission_profile_selection_actions(selection));
        } else if let (Some(approval), Some(permission_profile), Some(active_permission_profile)) = (
            approval,
            permission_profile.clone(),
            active_permission_profile.clone(),
        ) {
            accept_actions.extend(Self::approval_preset_actions(
                approval,
                permission_profile,
                active_permission_profile,
                mode_label.to_string(),
                ApprovalsReviewer::User,
            ));
        }

        let mut accept_and_remember_actions: Vec<SelectionAction> = Vec::new();
        accept_and_remember_actions.push(Box::new(|tx| {
            tx.send(AppEvent::UpdateWorldWritableWarningAcknowledged(true));
            tx.send(AppEvent::PersistWorldWritableWarningAcknowledged);
        }));
        if let Some(selection) = profile_selection {
            accept_and_remember_actions
                .extend(Self::permission_profile_selection_actions(selection));
        } else if let (Some(approval), Some(permission_profile), Some(active_permission_profile)) =
            (approval, permission_profile, active_permission_profile)
        {
            accept_and_remember_actions.extend(Self::approval_preset_actions(
                approval,
                permission_profile,
                active_permission_profile,
                mode_label.to_string(),
                ApprovalsReviewer::User,
            ));
        }

        let items = vec![
            SelectionItem {
                name: "Continue".to_string(),
                description: Some(format!("Apply {mode_label} for this session")),
                actions: accept_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Continue and don't warn again".to_string(),
                description: Some(format!("Enable {mode_label} and remember this choice")),
                actions: accept_and_remember_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header: Box::new(header),
            ..Default::default()
        });
    }

    #[cfg(not(target_os = "windows"))]
    pub(crate) fn open_world_writable_warning_confirmation(
        &mut self,
        _preset: Option<ApprovalPreset>,
        _profile_selection: Option<PermissionProfileSelection>,
        _sample_paths: Vec<String>,
        _extra_count: usize,
        _failed_scan: bool,
    ) {
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn open_windows_sandbox_enable_prompt(
        &mut self,
        preset: ApprovalPreset,
        profile_selection: Option<PermissionProfileSelection>,
    ) {
        use ratatui_macros::line;

        self.session_telemetry.counter(
            "codex.windows_sandbox.elevated_prompt_shown",
            /*inc*/ 1,
            &[],
        );

        let allow_unelevated =
            self.windows_sandbox_mode_allowed(WindowsSandboxModeToml::Unelevated);
        let setup_choice_is_required =
            !allow_unelevated || self.elevated_windows_sandbox_setup_required();
        let mut header = ColumnRenderable::new();
        header.push(*Box::new(
            Paragraph::new(if allow_unelevated {
                vec![
                    line!["Set up the Codex agent sandbox to protect your files and control network access. Learn more <https://developers.openai.com/codex/windows>"],
                ]
            } else {
                vec![
                    line!["Your organization requires the default Codex agent sandbox to continue. Set it up to protect your files and control network access."],
                    line!["Learn more <https://developers.openai.com/codex/windows>"],
                ]
            })
            .wrap(Wrap { trim: false }),
        ));

        let accept_otel = self.session_telemetry.clone();
        let legacy_otel = self.session_telemetry.clone();
        let legacy_preset = preset.clone();
        let legacy_profile_selection = profile_selection.clone();
        let quit_otel = self.session_telemetry.clone();
        let retry_preset = preset.clone();
        let retry_profile_selection = profile_selection.clone();
        let mut items = vec![SelectionItem {
            name: "Set up default sandbox (requires Administrator permissions)".to_string(),
            description: None,
            actions: vec![Box::new(move |tx| {
                accept_otel.counter(
                    "codex.windows_sandbox.elevated_prompt_accept",
                    /*inc*/ 1,
                    &[],
                );
                tx.send(AppEvent::BeginWindowsSandboxElevatedSetup {
                    preset: preset.clone(),
                    profile_selection: profile_selection.clone(),
                });
            })],
            dismiss_on_select: true,
            ..Default::default()
        }];
        if allow_unelevated {
            items.push(SelectionItem {
                name: "Use non-admin sandbox (higher risk if prompt injected)".to_string(),
                description: Some(LEGACY_SANDBOX_OPTION_DESCRIPTION.to_string()),
                actions: vec![Box::new(move |tx| {
                    legacy_otel.counter(
                        "codex.windows_sandbox.elevated_prompt_use_legacy",
                        /*inc*/ 1,
                        &[],
                    );
                    tx.send(AppEvent::BeginWindowsSandboxLegacySetup {
                        preset: legacy_preset.clone(),
                        profile_selection: legacy_profile_selection.clone(),
                    });
                })],
                dismiss_on_select: true,
                ..Default::default()
            });
        }
        items.push(SelectionItem {
            name: "Quit".to_string(),
            description: None,
            actions: vec![Box::new(move |tx| {
                quit_otel.counter(
                    "codex.windows_sandbox.elevated_prompt_quit",
                    /*inc*/ 1,
                    &[],
                );
                tx.send(AppEvent::Exit(ExitMode::ShutdownFirst));
            })],
            dismiss_on_select: true,
            ..Default::default()
        });

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: None,
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header: Box::new(header),
            on_cancel: setup_choice_is_required.then(|| {
                Box::new(move |tx: &AppEventSender| {
                    tx.send(AppEvent::OpenWindowsSandboxEnablePrompt {
                        preset: retry_preset.clone(),
                        profile_selection: retry_profile_selection.clone(),
                    });
                }) as _
            }),
            ..Default::default()
        });
    }

    #[cfg(all(not(target_os = "windows"), not(test)))]
    pub(crate) fn open_windows_sandbox_enable_prompt(
        &mut self,
        _preset: ApprovalPreset,
        _profile_selection: Option<PermissionProfileSelection>,
    ) {
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn open_windows_sandbox_fallback_prompt(
        &mut self,
        preset: ApprovalPreset,
        profile_selection: Option<PermissionProfileSelection>,
    ) {
        use ratatui_macros::line;

        let allow_unelevated =
            self.windows_sandbox_mode_allowed(WindowsSandboxModeToml::Unelevated);
        let setup_choice_is_required =
            !allow_unelevated || self.elevated_windows_sandbox_setup_required();
        let mut lines = Vec::new();
        lines.push(line![
            "Couldn't set up your sandbox with Administrator permissions".bold()
        ]);
        lines.push(line![""]);
        if allow_unelevated {
            lines.push(line![
                "You can still use Corbanu Terminal in a non-admin sandbox. It carries greater risk if prompt injected."
            ]);
            lines.push(line![
                "The non-admin sandbox blocks writes and deletes outside your workspace except in low-integrity folders, but it can't stop commands from reading your files and its network block can be bypassed.".bold()
            ]);
        } else {
            lines.push(line![
                "Your organization requires the default sandbox before Corbanu Terminal can continue."
            ]);
        }
        lines.push(line![
            "Learn more <https://developers.openai.com/codex/windows>"
        ]);

        let mut header = ColumnRenderable::new();
        header.push(*Box::new(Paragraph::new(lines).wrap(Wrap { trim: false })));

        let elevated_preset = preset.clone();
        let legacy_preset = preset;
        let retry_preset = elevated_preset.clone();
        let retry_profile_selection = profile_selection.clone();
        let elevated_profile_selection = profile_selection.clone();
        let legacy_profile_selection = profile_selection;
        let quit_otel = self.session_telemetry.clone();
        let mut items = vec![SelectionItem {
            name: "Try setting up admin sandbox again".to_string(),
            description: None,
            actions: vec![Box::new({
                let otel = self.session_telemetry.clone();
                let preset = elevated_preset;
                move |tx| {
                    otel.counter(
                        "codex.windows_sandbox.fallback_retry_elevated",
                        /*inc*/ 1,
                        &[],
                    );
                    tx.send(AppEvent::BeginWindowsSandboxElevatedSetup {
                        preset: preset.clone(),
                        profile_selection: elevated_profile_selection.clone(),
                    });
                }
            })],
            dismiss_on_select: true,
            ..Default::default()
        }];
        if allow_unelevated {
            items.push(SelectionItem {
                name: "Use Codex with non-admin sandbox".to_string(),
                description: Some(LEGACY_SANDBOX_OPTION_DESCRIPTION.to_string()),
                actions: vec![Box::new({
                    let otel = self.session_telemetry.clone();
                    let preset = legacy_preset;
                    move |tx| {
                        otel.counter(
                            "codex.windows_sandbox.fallback_use_legacy",
                            /*inc*/ 1,
                            &[],
                        );
                        tx.send(AppEvent::BeginWindowsSandboxLegacySetup {
                            preset: preset.clone(),
                            profile_selection: legacy_profile_selection.clone(),
                        });
                    }
                })],
                dismiss_on_select: true,
                ..Default::default()
            });
        }
        items.push(SelectionItem {
            name: "Quit".to_string(),
            description: None,
            actions: vec![Box::new(move |tx| {
                quit_otel.counter(
                    "codex.windows_sandbox.fallback_prompt_quit",
                    /*inc*/ 1,
                    &[],
                );
                tx.send(AppEvent::Exit(ExitMode::ShutdownFirst));
            })],
            dismiss_on_select: true,
            ..Default::default()
        });

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: None,
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header: Box::new(header),
            on_cancel: setup_choice_is_required.then(|| {
                Box::new(move |tx: &AppEventSender| {
                    tx.send(AppEvent::OpenWindowsSandboxFallbackPrompt {
                        preset: retry_preset.clone(),
                        profile_selection: retry_profile_selection.clone(),
                    });
                }) as _
            }),
            ..Default::default()
        });
    }

    #[cfg(all(not(target_os = "windows"), not(test)))]
    pub(crate) fn open_windows_sandbox_fallback_prompt(
        &mut self,
        _preset: ApprovalPreset,
        _profile_selection: Option<PermissionProfileSelection>,
    ) {
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn maybe_prompt_windows_sandbox_enable(&mut self, show_now: bool) {
        let windows_sandbox_level = crate::windows_sandbox::level_from_config(&self.config);
        let setup_is_required = windows_sandbox_level == WindowsSandboxLevel::Disabled
            || self.elevated_windows_sandbox_setup_required();
        if show_now
            && setup_is_required
            && let Some(preset) = builtin_approval_presets()
                .into_iter()
                .find(|preset| preset.id == "auto")
        {
            self.open_windows_sandbox_enable_prompt(preset, /*profile_selection*/ None);
        }
    }

    #[cfg(not(target_os = "windows"))]
    pub(crate) fn maybe_prompt_windows_sandbox_enable(&mut self, _show_now: bool) {}

    #[cfg(target_os = "windows")]
    pub(crate) fn show_windows_sandbox_setup_status(&mut self) {
        // While elevated sandbox setup runs, prevent typing so the user doesn't
        // accidentally queue messages that will run under an unexpected mode.
        self.bottom_pane.set_composer_input_enabled(
            /*enabled*/ false,
            Some("Input disabled until setup completes.".to_string()),
        );
        self.bottom_pane.ensure_status_indicator();
        self.bottom_pane
            .set_interrupt_hint_visible(/*visible*/ false);
        self.set_status(
            "Setting up sandbox...".to_string(),
            Some("Hang tight, this may take a few minutes".to_string()),
            StatusDetailsCapitalization::CapitalizeFirst,
            STATUS_DETAILS_DEFAULT_MAX_LINES,
        );
        self.request_redraw();
    }

    #[cfg(not(target_os = "windows"))]
    #[allow(dead_code)]
    pub(crate) fn show_windows_sandbox_setup_status(&mut self) {}

    #[cfg(target_os = "windows")]
    pub(crate) fn clear_windows_sandbox_setup_status(&mut self) {
        self.bottom_pane
            .set_composer_input_enabled(/*enabled*/ true, /*placeholder*/ None);
        self.bottom_pane.hide_status_indicator();
        self.request_redraw();
    }

    #[cfg(not(target_os = "windows"))]
    pub(crate) fn clear_windows_sandbox_setup_status(&mut self) {}

    /// #1506 item 2 (D262): at startup, asks whether to load the `.codex` or
    /// `.agents` folders of this project that appeared while a non-admin
    /// sandbox command ran. Until confirmed, Core ignores them. D332: the
    /// same for a `.git` that appeared or changed, which Core warns about.
    pub(crate) fn maybe_prompt_planted_project_metadata(&mut self) {
        // Unreadable records can't be confirmed; the startup warning says so.
        let codex_config::PlantedProjectMetadata::Known(planted) =
            codex_config::planted_project_metadata()
        else {
            return;
        };
        let here = planted_to_ask_about(planted, &self.config);
        // D420 (#1775): repositories that appeared or changed, same scope as
        // a `.git`.
        let git_changes = git_changes_to_ask_about(&self.config);
        if !here.is_empty() || !git_changes.is_empty() {
            self.open_planted_prompt(here, git_changes);
        }
    }

    pub(crate) fn open_planted_project_metadata_prompt(&mut self, paths: Vec<PathBuf>) {
        self.open_planted_prompt(paths, Vec::new());
    }

    /// The startup prompt for `paths` (recorded folders) and (D420)
    /// `git_changes`.
    pub(crate) fn open_planted_prompt(
        &mut self,
        mut paths: Vec<PathBuf>,
        git_changes: Vec<codex_config::PlantedGitChange>,
    ) {
        use ratatui_macros::line;

        // D332 (#1682): a `.git` isn't loaded by Corbanu Terminal, but git
        // runs its hooks and config outside the sandbox.
        let (git_dirs, folders): (Vec<&PathBuf>, Vec<&PathBuf>) = paths
            .iter()
            .partition(|path| codex_config::is_git_dir(path));
        let git_dirs: Vec<String> = git_dirs
            .iter()
            .map(|path| path.display().to_string())
            .chain(
                git_changes
                    .iter()
                    .map(codex_config::PlantedGitChange::describe),
            )
            .collect();
        let mut lines = vec![line!["Possibly planted project folders".bold()]];
        if !folders.is_empty() {
            lines.push(line![""]);
            lines.push(line![
                "These folders appeared while a non-admin Windows sandbox command ran, so a command may have planted them. Corbanu Terminal isn't loading their config, hooks or skills:"
            ]);
            for path in &folders {
                lines.push(line![format!("  {}", path.display()).cyan()]);
            }
        }
        // D283: a recorded folder that isn't `.codex`/`.agents` is one the
        // sandbox couldn't list when a command ended.
        let unlisted = folders.iter().any(|path| {
            !path.file_name().is_some_and(|name| {
                name.eq_ignore_ascii_case(".codex") || name.eq_ignore_ascii_case(".agents")
            })
        });
        if unlisted {
            lines.push(line![
                "A folder other than .codex or .agents couldn't be listed when a command ended, so nothing below it is loaded; make it readable and check it first."
            ]);
        }
        if !git_dirs.is_empty() {
            lines.push(line![""]);
            lines.push(line![
                "These git folders appeared or changed while a non-admin Windows sandbox command ran, so a command may have planted them. Git runs their hooks and config outside the sandbox whenever you run git there:"
            ]);
            for item in &git_dirs {
                lines.push(line![format!("  {item}").cyan()]);
            }
        }
        let git_only = folders.is_empty();
        lines.push(if git_only {
            line!["Check their hooks and config before you confirm them.".bold()]
        } else if git_dirs.is_empty() {
            line!["Check their contents before you load them.".bold()]
        } else {
            line![
                "Check their contents (for a .git, its hooks and config) before you load or confirm them."
                    .bold()
            ]
        });
        // Confirming covers the changed repositories too.
        paths.extend(git_changes.into_iter().map(|change| change.path));
        let mut header = ColumnRenderable::new();
        header.push(*Box::new(Paragraph::new(lines).wrap(Wrap { trim: false })));

        let (keep, confirm, confirm_description, confirmed) = if git_only {
            (
                "Keep warning me",
                "Confirm them",
                "I checked them; stop warning about them",
                "Confirmed.",
            )
        } else {
            (
                "Keep ignoring them",
                "Load them",
                "I checked them; load them from the next start",
                "Confirmed. Restart Corbanu Terminal to load them.",
            )
        };
        let items = vec![
            SelectionItem {
                name: keep.to_string(),
                description: Some("Ask again next time".to_string()),
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: confirm.to_string(),
                description: Some(confirm_description.to_string()),
                actions: vec![Box::new(move |tx| {
                    let message = match confirm_planted_project_metadata(&paths) {
                        Ok(()) => confirmed.to_string(),
                        Err(err) => format!("Couldn't confirm them: {err}"),
                    };
                    tx.send(AppEvent::InsertHistoryCell(Box::new(
                        crate::history_cell::new_info_event(message, /*hint*/ None),
                    )));
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
        ];
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: None,
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header: Box::new(header),
            ..Default::default()
        });
    }
}

/// D420 (#1775): the repositories that appeared or changed that the startup
/// prompt asks about, in the scope of [`planted_to_ask_about`]'s `.git`s.
fn git_changes_to_ask_about(config: &Config) -> Vec<codex_config::PlantedGitChange> {
    let changes = codex_config::planted_git_changes();
    if changes.is_empty() {
        return changes;
    }
    let cwd = config.cwd.as_path();
    let writable = codex_windows_sandbox::legacy_writable_roots(
        &config.permissions.file_system_sandbox_policy(),
        config.permissions.workspace_roots(),
        cwd,
    );
    codex_config::git_changes_to_show(changes, &writable, cwd)
}

/// The recorded folders the startup prompt asks about: those in the working
/// folder or a folder above it (where Core loads `.codex`/`.agents` from),
/// and (D424, #1775) any `.git` below the working folder or in a folder
/// non-admin sandbox commands may write to, since git runs anywhere.
pub(super) fn planted_to_ask_about(planted: Vec<PathBuf>, config: &Config) -> Vec<PathBuf> {
    let cwd = config.cwd.as_path();
    let writable = codex_windows_sandbox::legacy_writable_roots(
        &config.permissions.file_system_sandbox_policy(),
        config.permissions.workspace_roots(),
        cwd,
    );
    let git_dirs = codex_config::PlantedProjectMetadata::Known(planted.clone())
        .git_dirs_to_show(&writable, cwd);
    let cwd = dunce::canonicalize(cwd).unwrap_or_else(|_| cwd.into());
    planted
        .into_iter()
        .filter(|path| {
            git_dirs.contains(path)
                || path.parent().is_some_and(|parent| {
                    let parent = dunce::canonicalize(parent).unwrap_or_else(|_| parent.into());
                    cwd.starts_with(&parent)
                })
        })
        .collect()
}

#[cfg(target_os = "windows")]
fn confirm_planted_project_metadata(paths: &[PathBuf]) -> anyhow::Result<()> {
    for path in paths {
        codex_windows_sandbox::confirm_planted_project_metadata(path)?;
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn confirm_planted_project_metadata(_paths: &[PathBuf]) -> anyhow::Result<()> {
    Ok(())
}
