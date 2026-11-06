#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StatusAccountDisplay {
    ChatGpt {
        email: Option<String>,
        plan: Option<String>,
    },
    ApiKey {
        /// The environment variable the key is read from, such as
        /// `OPENAI_API_KEY`; `None` for a saved key.
        env_var: Option<String>,
    },
    /// #1563: `/providers` reports the current provider not configured, so
    /// `/status` must not claim a key. `provider` is its `/providers` name.
    NotConfigured { provider: String },
}
