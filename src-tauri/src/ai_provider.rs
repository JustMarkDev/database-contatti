use futures::future::BoxFuture;
use std::time::Duration;

#[derive(Debug, Clone)]
pub(crate) struct InferenceRequest {
    pub system_prompt: String,
    pub user_prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderError {
    pub message: String,
    pub retryable: bool,
    pub kind: ProviderErrorKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderErrorKind {
    #[cfg(test)]
    Transport,
    Timeout,
    Quota,
    Unavailable,
    Structural,
}

impl ProviderError {
    pub(crate) fn terminal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
            kind: ProviderErrorKind::Structural,
        }
    }

    #[cfg(test)]
    pub(crate) fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
            kind: ProviderErrorKind::Transport,
        }
    }

    pub(crate) fn timeout(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
            kind: ProviderErrorKind::Timeout,
        }
    }

    /// Credit / access denial: pause for a manual Riprendi after the user tops up.
    pub(crate) fn quota(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
            kind: ProviderErrorKind::Quota,
        }
    }

    /// Transient rate limit or overload: the run waits and retries on its own.
    pub(crate) fn rate_limited(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
            kind: ProviderErrorKind::Quota,
        }
    }

    pub(crate) fn unavailable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
            kind: ProviderErrorKind::Unavailable,
        }
    }

    pub(crate) fn is_auto_retryable_limit(&self) -> bool {
        self.kind == ProviderErrorKind::Quota && self.retryable
    }
}

/// How long to wait before the next provider attempt after a transient limit.
/// OpenCode usage windows need a longer pause than TypeSafe 429/529 blips.
pub(crate) fn provider_limit_backoff(message: &str, attempt: u32) -> Duration {
    let lower = message.to_lowercase();
    let base_secs = if lower.contains("opencode") || lower.contains("usage limit") {
        300_u64 // 5 minutes: subscription windows recover slowly
    } else {
        45_u64 // TypeSafe docs: short delay + exponential backoff
    };
    let shift = attempt.saturating_sub(1).min(3);
    Duration::from_secs(base_secs.saturating_mul(1_u64 << shift).min(900))
}

pub(crate) trait InferenceProvider: Send + Sync {
    fn provider_id(&self) -> &'static str;

    fn complete(
        &self,
        request: InferenceRequest,
    ) -> BoxFuture<'static, Result<String, ProviderError>>;

    fn take_last_usage(&self) -> Option<serde_json::Value> {
        None
    }
}

pub(crate) async fn complete_with_retry_detailed(
    provider: &dyn InferenceProvider,
    request: InferenceRequest,
    maximum_attempts: usize,
) -> Result<String, ProviderError> {
    let maximum_attempts = maximum_attempts.max(1);
    for attempt in 1..=maximum_attempts {
        match provider.complete(request.clone()).await {
            Ok(response) => return Ok(response),
            // Quota/rate limits belong to the run loop (longer waits). Short
            // retries here would burn a limited OpenCode window three times.
            Err(error)
                if error.retryable
                    && error.kind != ProviderErrorKind::Quota
                    && attempt < maximum_attempts =>
            {
                let wait = Duration::from_millis(2_000 * (1_u64 << (attempt - 1).min(5)));
                tokio::time::sleep(wait).await;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("the retry loop always returns")
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    pub(crate) struct ScriptedProvider {
        responses: Arc<Mutex<VecDeque<Result<String, ProviderError>>>>,
        requests: Arc<Mutex<Vec<InferenceRequest>>>,
    }

    impl ScriptedProvider {
        pub(crate) fn new(responses: Vec<Result<String, ProviderError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
            }
        }

        pub(crate) fn request_count(&self) -> usize {
            self.requests.lock().expect("requests lock").len()
        }
    }

    impl InferenceProvider for ScriptedProvider {
        fn provider_id(&self) -> &'static str {
            "scripted"
        }

        fn complete(
            &self,
            request: InferenceRequest,
        ) -> BoxFuture<'static, Result<String, ProviderError>> {
            let requests = self.requests.clone();
            let responses = self.responses.clone();
            Box::pin(async move {
                requests.lock().expect("requests lock").push(request);
                responses
                    .lock()
                    .expect("responses lock")
                    .pop_front()
                    .unwrap_or_else(|| Err(ProviderError::terminal("script exhausted")))
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::ScriptedProvider;
    use super::*;

    fn request() -> InferenceRequest {
        InferenceRequest {
            system_prompt: "policy".to_owned(),
            user_prompt: "record".to_owned(),
        }
    }

    #[tokio::test]
    async fn retries_transport_failures_but_not_terminal_provider_errors() {
        let provider = ScriptedProvider::new(vec![
            Err(ProviderError::retryable("temporary")),
            Ok("valid".to_owned()),
        ]);
        assert_eq!(
            complete_with_retry_detailed(&provider, request(), 2)
                .await
                .unwrap(),
            "valid"
        );
        assert_eq!(provider.request_count(), 2);

        let terminal = ScriptedProvider::new(vec![Err(ProviderError::terminal("bad request"))]);
        assert_eq!(
            complete_with_retry_detailed(&terminal, request(), 3)
                .await
                .unwrap_err(),
            ProviderError::terminal("bad request")
        );
        assert_eq!(terminal.request_count(), 1);
    }

    #[test]
    fn rate_limited_errors_are_auto_retryable_and_credit_is_not() {
        let limited = ProviderError::rate_limited("TypeSafe rate/overloaded (529)");
        assert!(limited.is_auto_retryable_limit());
        assert!(!ProviderError::quota("credito esaurito").is_auto_retryable_limit());
        assert_eq!(
            provider_limit_backoff("Limite OpenCode raggiunto", 1),
            Duration::from_secs(300)
        );
        assert_eq!(
            provider_limit_backoff("TypeSafe rate/overloaded (429)", 1),
            Duration::from_secs(45)
        );
        assert_eq!(
            provider_limit_backoff("TypeSafe rate/overloaded (429)", 3),
            Duration::from_secs(180)
        );
    }
}
