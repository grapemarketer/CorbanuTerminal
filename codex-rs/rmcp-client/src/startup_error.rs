use anyhow::Error;
use rmcp::service::ClientInitializeError;
use rmcp::transport::auth::AuthError;
use rmcp::transport::streamable_http_client::StreamableHttpError;

use crate::http_client_adapter::StreamableHttpClientAdapterError;

/// Converts an rmcp OAuth error into `anyhow::Error` without losing its reason.
///
/// rmcp >= 3.9 keeps the cause of a failed discovery request in
/// `AuthError::DiscoveryRequestFailed`'s `source()`, which `Display` (used for
/// startup and login errors) omits. The `AuthError` stays downcastable.
pub(crate) fn auth_error_with_reason(error: AuthError) -> Error {
    match &error {
        AuthError::DiscoveryRequestFailed { source, .. } => {
            let message = format!("{error}: {source}");
            Error::new(error).context(message)
        }
        _ => Error::new(error),
    }
}

/// Returns whether an RMCP client error indicates that authentication is required.
///
/// This does not distinguish first-time login from reauthentication.
/// Streamable HTTP initialization errors are stored inside RMCP's dynamic
/// transport error, which is not part of the standard error source chain.
pub fn is_authentication_required_error(error: &Error) -> bool {
    error.chain().any(|source| {
        source
            .downcast_ref::<AuthError>()
            .is_some_and(auth_error_requires_authentication)
            || source
                .downcast_ref::<ClientInitializeError>()
                .is_some_and(client_initialize_error_requires_authentication)
            || source
                .downcast_ref::<rmcp::service::ServiceError>()
                .is_some_and(service_error_requires_authentication)
            || source
                .downcast_ref::<crate::rmcp_client::ClientOperationError>()
                .is_some_and(|error| {
                    // The transparent operation wrapper forwards its source, skipping
                    // ServiceError itself in anyhow's ordinary error-chain traversal.
                    matches!(error, crate::rmcp_client::ClientOperationError::Service(error)
                    if service_error_requires_authentication(error))
                })
    })
}

fn service_error_requires_authentication(error: &rmcp::service::ServiceError) -> bool {
    let rmcp::service::ServiceError::TransportSend(error) = error else {
        return false;
    };
    error
        .error
        .downcast_ref::<StreamableHttpError<StreamableHttpClientAdapterError>>()
        .is_some_and(streamable_error_requires_authentication)
}

fn client_initialize_error_requires_authentication(mut error: &ClientInitializeError) -> bool {
    // rmcp >= 3.1.3 wraps a failed legacy fallback; the fallback carries the HTTP status.
    while let ClientInitializeError::LegacyFallbackFailed { fallback, .. } = error {
        error = fallback;
    }
    let ClientInitializeError::TransportError { error, .. } = error else {
        return false;
    };

    error
        .error
        .downcast_ref::<StreamableHttpError<StreamableHttpClientAdapterError>>()
        .is_some_and(streamable_error_requires_authentication)
}

fn streamable_error_requires_authentication(
    error: &StreamableHttpError<StreamableHttpClientAdapterError>,
) -> bool {
    match error {
        StreamableHttpError::AuthRequired(_) => true,
        StreamableHttpError::Auth(error) => auth_error_requires_authentication(error),
        _ => false,
    }
}

fn auth_error_requires_authentication(error: &AuthError) -> bool {
    matches!(
        error,
        AuthError::AuthorizationRequired | AuthError::TokenExpired
    )
}

#[cfg(test)]
mod tests {
    use std::any::TypeId;

    use rmcp::transport::DynamicTransportError;
    use rmcp::transport::streamable_http_client::AuthRequiredError;

    use super::*;

    fn initialize_transport_error(
        error: StreamableHttpError<StreamableHttpClientAdapterError>,
    ) -> ClientInitializeError {
        ClientInitializeError::TransportError {
            error: DynamicTransportError::from_parts(
                "streamable_http",
                TypeId::of::<()>(),
                Box::new(error),
            ),
            context: "send initialize request".into(),
        }
    }

    #[test]
    fn authentication_required_looks_through_a_failed_legacy_fallback() {
        let fallback_failed = |fallback| {
            Error::new(ClientInitializeError::LegacyFallbackFailed {
                discover: Box::new(ClientInitializeError::ConnectionClosed(
                    "discover".to_string(),
                )),
                fallback: Box::new(fallback),
            })
        };

        assert!(is_authentication_required_error(&fallback_failed(
            initialize_transport_error(StreamableHttpError::AuthRequired(AuthRequiredError::new(
                "Bearer".to_string()
            ),)),
        )));
        assert!(!is_authentication_required_error(&fallback_failed(
            initialize_transport_error(StreamableHttpError::UnexpectedServerResponse(
                "HTTP 500".into(),
            )),
        )));
    }
}
