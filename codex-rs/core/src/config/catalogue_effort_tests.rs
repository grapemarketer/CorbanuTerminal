use super::ConfigBuilder;
use codex_protocol::openai_models::ChatReasoningEffortProtocol;
use codex_protocol::openai_models::ReasoningEffort;
use pretty_assertions::assert_eq;
use tempfile::tempdir;
use toml::Value;

#[tokio::test]
async fn exact_catalogue_efforts_survive_config_load() {
    let catalogue = codex_models_manager::bundled_models_response().expect("catalogue");
    // #1157: one working folder for every load; no command runs in it.
    let cwd = tempdir().expect("cwd");
    for model in catalogue.models.iter().filter(|model| {
        model.slug.starts_with("glm-")
            && model.chat_completions.reasoning_effort_protocol
                != ChatReasoningEffortProtocol::ProviderDefault
    }) {
        for level in &model.supported_reasoning_levels {
            let home = tempdir().expect("home");
            let config = ConfigBuilder::default()
                .codex_home(home.path().to_path_buf())
                .harness_overrides(super::ConfigOverrides {
                    cwd: Some(cwd.path().to_path_buf()),
                    ..Default::default()
                })
                .cli_overrides(vec![
                    (
                        "model_provider".to_string(),
                        Value::String("zai".to_string()),
                    ),
                    ("model".to_string(), Value::String(model.slug.clone())),
                    (
                        "model_reasoning_effort".to_string(),
                        Value::String(level.effort.as_str().to_string()),
                    ),
                ])
                .build()
                .await
                .expect("exact model configuration");
            assert_eq!(
                (config.model, config.model_reasoning_effort),
                (Some(model.slug.clone()), Some(level.effort.clone())),
            );
        }
    }
}

#[tokio::test]
async fn legacy_glm_reasoning_choices_survive_config_load_and_troll_role() {
    let cwd = tempdir().expect("cwd");
    for (provider, model, expected_standard) in [
        ("zai", "glm-5.2", ReasoningEffort::Medium),
        ("ambient", "z-ai/glm-5.2", ReasoningEffort::Medium),
        // Corbanu Plan lists the same GLM 5.2 row (Standard and Deep) (#1476).
        ("pfterminal-plan", "z-ai/glm-5.2", ReasoningEffort::Medium),
        // An unrelated legacy Ambient model keeps its existing normalization.
        ("ambient", "ambient/large", ReasoningEffort::XHigh),
        ("pfterminal-plan", "ambient/large", ReasoningEffort::XHigh),
    ] {
        for (effort, expected_effort) in [
            (Some(ReasoningEffort::Medium), Some(expected_standard)),
            (Some(ReasoningEffort::XHigh), Some(ReasoningEffort::XHigh)),
            (None, None),
        ] {
            let home = tempdir().expect("home");
            let mut overrides = vec![
                (
                    "model_provider".to_string(),
                    Value::String(provider.to_string()),
                ),
                ("model".to_string(), Value::String(model.to_string())),
            ];
            if let Some(effort) = &effort {
                overrides.push((
                    "model_reasoning_effort".to_string(),
                    Value::String(effort.as_str().to_string()),
                ));
            }
            let mut config = ConfigBuilder::default()
                .codex_home(home.path().to_path_buf())
                .harness_overrides(super::ConfigOverrides {
                    cwd: Some(cwd.path().to_path_buf()),
                    ..Default::default()
                })
                .cli_overrides(overrides)
                .build()
                .await
                .expect("legacy GLM configuration");
            super::apply_agent_role_to_config(&mut config, Some("troll"))
                .await
                .expect("spawned Troll role configuration");
            assert_eq!(
                (
                    config.model_provider_id,
                    config.model,
                    config.model_reasoning_effort,
                ),
                (
                    provider.to_string(),
                    Some(model.to_string()),
                    expected_effort
                ),
            );
        }
    }
}
