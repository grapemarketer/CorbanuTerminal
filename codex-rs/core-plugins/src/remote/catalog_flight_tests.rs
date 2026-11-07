use super::super::RemotePluginCatalogCacheMode;
use super::super::fetch_and_cache_remote_plugin_catalog;
use super::super::fetch_directory_plugins_for_scope_with_cache;
use super::super::invalidate_cached_remote_plugin_catalog_scopes;
use super::*;
use pretty_assertions::assert_eq;
use std::time::Duration;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

const LIST_PATH: &str = "/backend-api/ps/plugins/list";

struct Fixture {
    server: MockServer,
    codex_home: TempDir,
    config: RemotePluginServiceConfig,
    auth: CodexAuth,
}

impl Fixture {
    async fn new() -> Self {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(LIST_PATH))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({
                        "plugins": [],
                        "pagination": {"next_page_token": null},
                    }))
                    .set_delay(Duration::from_millis(500)),
            )
            .mount(&server)
            .await;
        let config = RemotePluginServiceConfig::new(
            format!("{}/backend-api", server.uri()),
            crate::test_support::test_http_client_factory(),
        );
        Self {
            server,
            codex_home: tempfile::tempdir().expect("create codex home"),
            config,
            auth: CodexAuth::create_dummy_chatgpt_auth_for_testing(),
        }
    }

    async fn list_requests(&self) -> usize {
        self.server
            .received_requests()
            .await
            .expect("wiremock records requests")
            .iter()
            .filter(|request| request.url.path() == LIST_PATH)
            .count()
    }

    async fn wait_for_list_requests(&self, expected: usize) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while self.list_requests().await < expected {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("catalog request should arrive");
    }

    async fn list(&self, mode: RemotePluginCatalogCacheMode) {
        fetch_directory_plugins_for_scope_with_cache(
            Some(self.codex_home.path()),
            &self.config,
            &self.auth,
            RemotePluginScope::Global,
            mode,
        )
        .await
        .expect("catalog list should succeed");
    }

    async fn refresh(&self, requested_at: Instant) {
        fetch_and_cache_remote_plugin_catalog(
            self.codex_home.path(),
            &self.config,
            Some(&self.auth),
            RemotePluginScope::Global,
            requested_at,
        )
        .await
        .expect("catalog refresh should succeed");
    }
}

#[tokio::test]
async fn startup_refresh_and_concurrent_lists_download_the_catalog_once() {
    let fixture = Fixture::new().await;
    let startup = Instant::now();

    tokio::join!(
        fixture.refresh(startup),
        fixture.list(RemotePluginCatalogCacheMode::PreferCache),
        fixture.list(RemotePluginCatalogCacheMode::PreferCache),
        fixture.list(RemotePluginCatalogCacheMode::PreferCache),
    );

    assert_eq!(fixture.list_requests().await, 1);
}

#[tokio::test]
async fn startup_refresh_reuses_a_download_completed_after_it_was_requested() {
    let fixture = Fixture::new().await;
    let startup = Instant::now();
    fixture
        .list(RemotePluginCatalogCacheMode::PreferCache)
        .await;

    fixture.refresh(startup).await;
    assert_eq!(fixture.list_requests().await, 1);

    // A refresh requested after that download still fetches.
    fixture.refresh(Instant::now()).await;
    assert_eq!(fixture.list_requests().await, 2);
}

#[tokio::test]
async fn force_refetch_does_not_join_a_download_that_started_earlier() {
    let fixture = Arc::new(Fixture::new().await);
    let earlier = {
        let fixture = Arc::clone(&fixture);
        tokio::spawn(async move {
            fixture
                .list(RemotePluginCatalogCacheMode::PreferCache)
                .await
        })
    };
    fixture.wait_for_list_requests(/*expected*/ 1).await;

    fixture
        .list(RemotePluginCatalogCacheMode::ForceRefetch)
        .await;
    earlier.await.expect("earlier list task");

    assert_eq!(fixture.list_requests().await, 2);
}

#[tokio::test]
async fn invalidation_stops_later_callers_joining_an_earlier_download() {
    let fixture = Arc::new(Fixture::new().await);
    let earlier = {
        let fixture = Arc::clone(&fixture);
        tokio::spawn(async move {
            fixture
                .list(RemotePluginCatalogCacheMode::PreferCache)
                .await
        })
    };
    fixture.wait_for_list_requests(/*expected*/ 1).await;

    invalidate_cached_remote_plugin_catalog_scopes(
        fixture.codex_home.path(),
        &fixture.config,
        Some(&fixture.auth),
        &[RemotePluginScope::Global],
    );
    fixture
        .list(RemotePluginCatalogCacheMode::PreferCache)
        .await;
    earlier.await.expect("earlier list task");

    assert_eq!(fixture.list_requests().await, 2);
}

#[tokio::test]
async fn a_download_started_before_invalidation_does_not_rewrite_the_cache() {
    let fixture = Arc::new(Fixture::new().await);
    let earlier = {
        let fixture = Arc::clone(&fixture);
        tokio::spawn(async move {
            fixture
                .list(RemotePluginCatalogCacheMode::PreferCache)
                .await
        })
    };
    fixture.wait_for_list_requests(/*expected*/ 1).await;

    invalidate_cached_remote_plugin_catalog_scopes(
        fixture.codex_home.path(),
        &fixture.config,
        Some(&fixture.auth),
        &[RemotePluginScope::Global],
    );
    earlier.await.expect("earlier list task");

    assert!(
        catalog_cache::load_cached_directory_plugins(
            fixture.codex_home.path(),
            &fixture.config,
            &fixture.auth,
            RemotePluginScope::Global,
        )
        .is_none()
    );
}

#[tokio::test]
async fn joined_callers_fetch_for_themselves_when_the_shared_download_is_cancelled() {
    let fixture = Arc::new(Fixture::new().await);
    let leader = {
        let fixture = Arc::clone(&fixture);
        tokio::spawn(async move {
            fixture
                .list(RemotePluginCatalogCacheMode::PreferCache)
                .await
        })
    };
    fixture.wait_for_list_requests(/*expected*/ 1).await;
    let follower = {
        let fixture = Arc::clone(&fixture);
        tokio::spawn(async move {
            fixture
                .list(RemotePluginCatalogCacheMode::PreferCache)
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(fixture.list_requests().await, 1);

    leader.abort();
    assert!(leader.await.is_err_and(|err| err.is_cancelled()));
    tokio::time::timeout(Duration::from_secs(10), follower)
        .await
        .expect("follower should finish")
        .expect("follower list task");

    assert_eq!(fixture.list_requests().await, 2);
}
