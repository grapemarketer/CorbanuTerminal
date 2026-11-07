use super::CLAUDE_REFRESH_LOCK_FILE;
use super::PlatformCredentialStore;
use super::acquire_refresh_lock;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use pretty_assertions::assert_eq;
use std::fs::OpenOptions;
use std::task::Poll;

#[tokio::test]
async fn issue928_cli_refresh_lock_releases_with_retained_descriptor() -> Result<()> {
    let config = tempfile::tempdir()?;
    let path = config.path().join(CLAUDE_REFRESH_LOCK_FILE);
    let contents = b"owned lock fixture";
    std::fs::write(&path, contents)?;
    let held =
        acquire_refresh_lock(config.path(), PlatformCredentialStore::CredentialsFile).await?;
    let retained = held.try_clone()?;
    let contender = OpenOptions::new().read(true).write(true).open(&path)?;
    assert!(matches!(
        contender.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));

    drop(held);

    assert!(
        contender.try_lock().is_ok(),
        "refresh owner drop must release with a duplicate still open"
    );
    assert_eq!(std::fs::read(&path)?, contents);
    assert_eq!(retained.metadata()?.len(), contents.len() as u64);
    contender.unlock()?;
    Ok(())
}

#[tokio::test]
async fn issue928_cli_refresh_lock_releases_on_scope_error() -> Result<()> {
    let config = tempfile::tempdir()?;
    let path = config.path().join(CLAUDE_REFRESH_LOCK_FILE);
    let mut retained = None;
    let outcome: Result<()> = async {
        let held =
            acquire_refresh_lock(config.path(), PlatformCredentialStore::CredentialsFile).await?;
        retained = Some(held.try_clone()?);
        Err(anyhow!("owned refresh-scope fixture error"))
    }
    .await;
    assert!(outcome.is_err());
    let retained = retained.context("the actual owner must have been acquired")?;
    let contender = OpenOptions::new().read(true).write(true).open(&path)?;

    assert!(
        contender.try_lock().is_ok(),
        "scope error must release with a duplicate still open"
    );
    retained.metadata()?;
    contender.unlock()?;
    Ok(())
}

#[tokio::test]
async fn issue928_cli_refresh_lock_releases_when_owned_future_is_cancelled() -> Result<()> {
    let config = tempfile::tempdir()?;
    let path = config.path().join(CLAUDE_REFRESH_LOCK_FILE);
    let held =
        acquire_refresh_lock(config.path(), PlatformCredentialStore::CredentialsFile).await?;
    let retained = held.try_clone()?;
    let contender = OpenOptions::new().read(true).write(true).open(&path)?;
    let mut owned_future = Box::pin(async move {
        let _held = held;
        std::future::pending::<()>().await;
    });
    assert!(matches!(
        futures::poll!(owned_future.as_mut()),
        Poll::Pending
    ));
    assert!(matches!(
        contender.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));

    drop(owned_future);

    assert!(
        contender.try_lock().is_ok(),
        "cancelling the owner must release with a duplicate still open"
    );
    retained.metadata()?;
    contender.unlock()?;
    Ok(())
}

#[tokio::test]
async fn issue928_cli_refresh_lock_duplicate_close_preserves_live_owner() -> Result<()> {
    let config = tempfile::tempdir()?;
    let path = config.path().join(CLAUDE_REFRESH_LOCK_FILE);
    let held =
        acquire_refresh_lock(config.path(), PlatformCredentialStore::CredentialsFile).await?;
    let duplicate = held.try_clone()?;
    let contender = OpenOptions::new().read(true).write(true).open(&path)?;

    drop(duplicate);

    assert!(matches!(
        contender.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    drop(held);
    assert!(contender.try_lock().is_ok());
    contender.unlock()?;
    Ok(())
}

#[tokio::test]
async fn issue928_cli_refresh_lock_acquisition_error_preserves_input() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let config_path = fixture.path().join("config-is-a-file");
    let contents = b"owned acquisition-error fixture";
    std::fs::write(&config_path, contents)?;

    let outcome =
        acquire_refresh_lock(&config_path, PlatformCredentialStore::CredentialsFile).await;

    assert!(outcome.is_err());
    assert_eq!(std::fs::read(&config_path)?, contents);
    Ok(())
}
