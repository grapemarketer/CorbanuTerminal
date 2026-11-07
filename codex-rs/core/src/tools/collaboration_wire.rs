//! Collaboration wire compatibility, independent of provider identity and authentication.
use crate::session::turn_context::TurnContext;
use codex_model_provider_info::OPENAI_PROVIDER_ID;

/// OpenAI's regional processing domain prefixes (`<prefix>.api.openai.com`), as listed in
/// the OpenAI platform data controls guide (checked 2026-10-28). New regions need adding here.
const OPENAI_REGIONAL_DOMAIN_PREFIXES: [&str; 10] =
    ["ae", "au", "ca", "eu", "gb", "in", "jp", "kr", "sg", "us"];

/// `api.openai.com` or a documented regional domain, with at most one trailing root dot.
fn is_official_openai_api_host(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    host == "api.openai.com"
        || host
            .strip_suffix(".api.openai.com")
            .is_some_and(|prefix| OPENAI_REGIONAL_DOMAIN_PREFIXES.contains(&prefix))
}

/// The collaboration wire contract follows the endpoint, not a custom display name.
/// Keep this narrower than provider identity: it must not grant OpenAI auth or capabilities.
pub(super) fn uses_openai_reserved_collaboration_schema(turn_context: &TurnContext) -> bool {
    let provider = turn_context.provider.info();
    if provider.is_openai() {
        return true;
    }
    provider.wire_api == codex_model_provider_info::WireApi::Responses
        && provider.base_url.as_deref().is_some_and(|base_url| {
            url::Url::parse(base_url).is_ok_and(|url| {
                url.scheme() == "https"
                    && url.host_str().is_some_and(is_official_openai_api_host)
                    && url.port_or_known_default() == Some(443)
                    && url.path().trim_end_matches('/') == "/v1"
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
            })
        })
}

/// Select the existing message codec without changing the configured provider or account.
/// Recipients keep their existing fail-closed identity check and can use plaintext adapters.
pub(super) fn message_source_provider_id(turn_context: &TurnContext) -> &str {
    if uses_openai_reserved_collaboration_schema(turn_context) {
        OPENAI_PROVIDER_ID
    } else {
        &turn_context.config.model_provider_id
    }
}
