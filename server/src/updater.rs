use std::{
    collections::HashSet,
    env,
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use futures_util::StreamExt;
use reqwest::{
    Client, Url,
    header::{ACCEPT, HeaderMap, HeaderValue, USER_AGENT},
    redirect::{Attempt, Policy},
};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::AsyncWriteExt,
    sync::{Mutex, RwLock, mpsc},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zip::ZipArchive;

use crate::{
    config::UpdatePolicy,
    process_tree::{BoundedProcessOptions, run_bounded},
    update_bootstrap::{
        READINESS_NONCE_ENV, SERVER_RESTART_CAPABILITY_ENV, SUPERVISED_WORKER_ENV,
        persist_pending_activation, remove_matching_pending_activation,
    },
    update_fs::{
        UpdateFileLock, ensure_private_directory, safe_remove_tree, validate_regular_directory,
    },
    update_manifest::{
        current_release_target, executable_name, expected_archive_name, expected_archive_root,
        is_official_current_package, validate_package_layout,
    },
};

const GITHUB_LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/bproject07/Codex-web/releases/latest";
const GITHUB_REPOSITORY: &str = "bproject07/Codex-web";
const GITHUB_REPOSITORY_ID: u64 = 1_312_275_218;
const GITHUB_ACTIONS_BOT_ID: u64 = 41_898_282;
const CHECKSUM_ASSET_NAME: &str = "SHA256SUMS.txt";
const UPDATE_SCHEMA_VERSION: u32 = 1;
const API_RESPONSE_LIMIT: u64 = 1024 * 1024;
const CHECKSUM_LIMIT: u64 = 64 * 1024;
const ARCHIVE_DOWNLOAD_LIMIT: u64 = 256 * 1024 * 1024;
const EXTRACTED_BYTES_LIMIT: u64 = 512 * 1024 * 1024;
const ARCHIVE_ENTRY_LIMIT: usize = 20_000;
const ARCHIVE_PATH_LIMIT: usize = 4 * 1024;
const MAX_REDIRECTS: usize = 5;
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const INITIAL_CHECK_DELAY: Duration = Duration::from_secs(2);
const STAGED_VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const STAGED_VERSION_OUTPUT_LIMIT: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateState {
    Disabled,
    Checking,
    UpToDate,
    Available,
    Downloading,
    Verifying,
    Staged,
    Restarting,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub schema_version: u32,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub state: UpdateState,
    pub install_supported: bool,
    pub install_reason: Option<String>,
    pub release_url: Option<String>,
    pub progress_percent: Option<u8>,
    pub error: Option<String>,
    pub checked_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UpdateActivation {
    pub request_id: Uuid,
    pub source_version: Version,
    pub version: Version,
    pub package_root: PathBuf,
}

#[derive(Clone)]
pub struct UpdateManager {
    inner: Arc<UpdateManagerInner>,
}

struct UpdateManagerInner {
    client: Client,
    state_dir: PathBuf,
    current_version: Version,
    policy: UpdatePolicy,
    package_support: Result<(), String>,
    status: RwLock<UpdateStatus>,
    release: RwLock<Option<VerifiedRelease>>,
    operation: Arc<Mutex<()>>,
    activation_tx: mpsc::Sender<UpdateActivation>,
}

#[derive(Debug, Clone)]
struct VerifiedRelease {
    version: Version,
    release_url: String,
    immutable: bool,
    archive: VerifiedAsset,
    checksums: VerifiedAsset,
}

#[derive(Debug, Clone)]
struct VerifiedAsset {
    name: String,
    url: Url,
    size: u64,
    sha256: [u8; 32],
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    target_commitish: String,
    author: GitHubActor,
    draft: bool,
    prerelease: bool,
    #[serde(default)]
    immutable: bool,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubActor {
    login: String,
    id: u64,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    state: String,
    digest: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubAttestationResponse {
    attestations: Vec<GitHubAttestation>,
}

#[derive(Debug, Deserialize)]
struct GitHubAttestation {
    repository_id: u64,
}

impl UpdateManager {
    pub fn new(
        state_dir: PathBuf,
        policy: UpdatePolicy,
        activation_tx: mpsc::Sender<UpdateActivation>,
    ) -> Result<Self> {
        let current_version =
            Version::parse(env!("CARGO_PKG_VERSION")).context("invalid build version")?;
        let executable =
            std::env::current_exe().context("failed to locate the running executable")?;
        let package_support = is_official_current_package(&executable).map_err(|_| {
            "Source/development build — install an official release package once; future releases can then update from Settings.".to_owned()
        });
        let client = Client::builder()
            .default_headers(github_headers()?)
            .redirect(Policy::custom(redirect_policy))
            .timeout(HTTP_TIMEOUT)
            .build()
            .context("failed to initialize the update HTTP client")?;
        let disabled = policy == UpdatePolicy::Off;
        let install_supported = !disabled && package_support.is_ok();
        let install_reason = if disabled {
            Some("Automatic update checks are disabled by the server operator.".to_owned())
        } else {
            package_support.as_ref().err().cloned()
        };

        Ok(Self {
            inner: Arc::new(UpdateManagerInner {
                client,
                state_dir,
                current_version: current_version.clone(),
                policy,
                package_support,
                status: RwLock::new(UpdateStatus {
                    schema_version: UPDATE_SCHEMA_VERSION,
                    current_version: current_version.to_string(),
                    latest_version: None,
                    state: if disabled {
                        UpdateState::Disabled
                    } else {
                        UpdateState::Checking
                    },
                    install_supported,
                    install_reason,
                    release_url: None,
                    progress_percent: None,
                    error: None,
                    checked_at: None,
                }),
                release: RwLock::new(None),
                operation: Arc::new(Mutex::new(())),
                activation_tx,
            }),
        })
    }

    pub async fn status(&self) -> UpdateStatus {
        self.inner.status.read().await.clone()
    }

    pub fn spawn_background_checks(&self, shutdown: CancellationToken) {
        if self.inner.policy == UpdatePolicy::Off {
            return;
        }
        let manager = self.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(INITIAL_CHECK_DELAY) => {}
            }

            loop {
                if let Err(error) = manager.check_now().await {
                    tracing::debug!(%error, "automatic update check failed");
                }
                tokio::select! {
                    _ = shutdown.cancelled() => return,
                    _ = tokio::time::sleep(CHECK_INTERVAL) => {}
                }
            }
        });
    }

    pub async fn check_now(&self) -> Result<UpdateStatus> {
        if self.inner.policy == UpdatePolicy::Off {
            return Ok(self.status().await);
        }
        let operation = self
            .inner
            .operation
            .clone()
            .try_lock_owned()
            .map_err(|_| anyhow::anyhow!("an update operation is already running"))?;
        self.replace_status(UpdateState::Checking, None, None).await;

        let result = self.fetch_latest_release().await;
        drop(operation);
        match result {
            Ok(release) => {
                let checked_at = Some(rfc3339_now());
                if release.version <= self.inner.current_version {
                    *self.inner.release.write().await = None;
                    let mut status = self.inner.status.write().await;
                    status.latest_version = Some(release.version.to_string());
                    status.release_url = Some(release.release_url);
                    status.state = UpdateState::UpToDate;
                    status.progress_percent = None;
                    status.error = None;
                    status.checked_at = checked_at;
                    self.apply_install_support(&mut status, true);
                } else {
                    let immutable = release.immutable;
                    let latest_version = release.version.to_string();
                    let release_url = release.release_url.clone();
                    *self.inner.release.write().await = Some(release);
                    let mut status = self.inner.status.write().await;
                    status.latest_version = Some(latest_version);
                    status.release_url = Some(release_url);
                    status.state = UpdateState::Available;
                    status.progress_percent = None;
                    status.error = None;
                    status.checked_at = checked_at;
                    self.apply_install_support(&mut status, immutable);
                }
                Ok(self.status().await)
            }
            Err(error) => {
                self.fail_status(&error).await;
                Err(error)
            }
        }
    }

    pub async fn begin_apply(
        &self,
        expected_version: &str,
        confirm_session_termination: bool,
    ) -> Result<()> {
        if !confirm_session_termination {
            bail!("session termination must be explicitly confirmed");
        }
        if self.inner.policy == UpdatePolicy::Off {
            bail!("updates are disabled by server policy");
        }
        self.inner
            .package_support
            .as_ref()
            .map_err(|reason| anyhow::anyhow!(reason.clone()))?;
        let expected =
            Version::parse(expected_version).context("expectedVersion is not valid SemVer")?;
        let release = self
            .inner
            .release
            .read()
            .await
            .clone()
            .context("check for an available update first")?;
        if release.version != expected {
            bail!("the selected release is no longer the checked release");
        }
        if release.version <= self.inner.current_version {
            bail!("refusing to install the current version or a downgrade");
        }
        if !release.immutable {
            bail!("the GitHub release is not immutable; automatic installation is disabled");
        }

        let operation = self
            .inner
            .operation
            .clone()
            .try_lock_owned()
            .map_err(|_| anyhow::anyhow!("an update operation is already running"))?;
        self.replace_status(UpdateState::Downloading, Some(0), None)
            .await;
        let manager = self.clone();
        tokio::spawn(async move {
            let _operation = operation;
            if let Err(error) = manager.prepare_and_activate(release).await {
                tracing::error!(%error, "update preparation failed");
                manager.fail_status(&error).await;
            }
        });
        Ok(())
    }

    async fn fetch_latest_release(&self) -> Result<VerifiedRelease> {
        let response = self
            .inner
            .client
            .get(GITHUB_LATEST_RELEASE_API)
            .send()
            .await
            .context("GitHub release check failed")?;
        if !response.status().is_success() {
            bail!(
                "GitHub release check returned HTTP {}",
                response.status().as_u16()
            );
        }
        let bytes = bounded_response(response, API_RESPONSE_LIMIT).await?;
        let release: GitHubRelease =
            serde_json::from_slice(&bytes).context("GitHub returned invalid release metadata")?;
        if release.draft || release.prerelease {
            bail!("GitHub latest release is not a published stable release");
        }
        if release.target_commitish != "main"
            || release.author.login != "github-actions[bot]"
            || release.author.id != GITHUB_ACTIONS_BOT_ID
            || release.author.kind != "Bot"
        {
            bail!("GitHub latest release was not published by the official release workflow");
        }
        let version = release
            .tag_name
            .strip_prefix('v')
            .context("release tag must start with v")
            .and_then(|value| Version::parse(value).context("release tag is not valid SemVer"))?;
        if !version.pre.is_empty() || !version.build.is_empty() {
            bail!("pre-release and build metadata versions are not accepted");
        }

        let archive_name = expected_archive_name(&version)?;
        let archive = select_asset(
            &release.assets,
            &archive_name,
            &version,
            ARCHIVE_DOWNLOAD_LIMIT,
        )?;
        let checksums = select_asset(
            &release.assets,
            CHECKSUM_ASSET_NAME,
            &version,
            CHECKSUM_LIMIT,
        )?;
        self.require_asset_attestation(&archive).await?;
        self.require_asset_attestation(&checksums).await?;

        Ok(VerifiedRelease {
            release_url: format!("https://github.com/{GITHUB_REPOSITORY}/releases/tag/v{version}"),
            version,
            immutable: release.immutable,
            archive,
            checksums,
        })
    }

    async fn prepare_and_activate(&self, release: VerifiedRelease) -> Result<()> {
        let updates_root = ensure_updates_root(&self.inner.state_dir)?;
        let _file_lock = UpdateFileLock::acquire(&updates_root)?;
        let work_root =
            updates_root
                .join("staging")
                .join(format!("v{}-{}", release.version, Uuid::new_v4()));
        ensure_private_directory(&work_root)?;

        let result = self
            .download_verify_and_extract(&release, &updates_root, &work_root)
            .await;
        let cleanup_result = safe_remove_tree(&work_root, &updates_root);
        let package_root = match result {
            Ok(package_root) => {
                if let Err(error) = cleanup_result {
                    tracing::warn!(%error, "temporary update directory could not be removed");
                }
                package_root
            }
            Err(error) => {
                if let Err(cleanup_error) = cleanup_result {
                    tracing::warn!(%cleanup_error, "failed update staging directory could not be removed");
                }
                return Err(error);
            }
        };

        self.replace_status(UpdateState::Staged, Some(100), None)
            .await;
        let activation = UpdateActivation {
            request_id: Uuid::new_v4(),
            source_version: self.inner.current_version.clone(),
            version: release.version,
            package_root,
        };
        persist_pending_activation(&self.inner.state_dir, &activation)
            .context("failed to persist the prepared update activation")?;
        drop(_file_lock);
        self.replace_status(UpdateState::Restarting, Some(100), None)
            .await;
        if let Err(send_error) = self.inner.activation_tx.send(activation.clone()).await {
            let cleanup = (|| {
                let updates_root = ensure_updates_root(&self.inner.state_dir)?;
                let _lock = UpdateFileLock::acquire(&updates_root)?;
                remove_matching_pending_activation(&self.inner.state_dir, &activation.request_id)
            })();
            if let Err(cleanup_error) = cleanup {
                bail!(
                    "the server is no longer accepting an update restart ({send_error}); \
                     its matching pending activation could not be removed ({cleanup_error:#})"
                );
            }
            bail!("the server is no longer accepting an update restart");
        }
        Ok(())
    }

    async fn download_verify_and_extract(
        &self,
        release: &VerifiedRelease,
        updates_root: &Path,
        work_root: &Path,
    ) -> Result<PathBuf> {
        self.require_asset_attestation(&release.archive).await?;
        self.require_asset_attestation(&release.checksums).await?;
        self.replace_status(UpdateState::Downloading, Some(0), None)
            .await;
        let checksum_path = work_root.join(CHECKSUM_ASSET_NAME);
        let checksum_digest = self
            .download_asset(&release.checksums, &checksum_path, false)
            .await?;
        if checksum_digest != release.checksums.sha256 {
            bail!("SHA256SUMS.txt does not match its GitHub asset digest");
        }

        let archive_path = work_root.join(&release.archive.name);
        let archive_digest = self
            .download_asset(&release.archive, &archive_path, true)
            .await?;
        if archive_digest != release.archive.sha256 {
            bail!("release archive does not match its GitHub asset digest");
        }

        let checksum_bytes = fs::read(&checksum_path).context("failed to read SHA256SUMS.txt")?;
        let listed_digest = checksum_for_asset(&checksum_bytes, &release.archive.name)?;
        if listed_digest != archive_digest {
            bail!("release archive does not match SHA256SUMS.txt");
        }

        self.replace_status(UpdateState::Verifying, Some(100), None)
            .await;
        let version = release.version.clone();
        let target = current_release_target()?.to_owned();
        let archive_path_for_task = archive_path.clone();
        let extraction_root = work_root.join("extracted");
        let extracted = tokio::task::spawn_blocking(move || {
            extract_release_archive(&archive_path_for_task, &extraction_root, &version, &target)
        })
        .await
        .context("release extraction task failed")??;

        let releases_root = updates_root.join("releases");
        ensure_private_directory(&releases_root)?;
        let final_root = releases_root.join(format!("v{}", release.version));
        if final_root.exists() {
            safe_remove_tree(&final_root, &releases_root)
                .context("failed to remove an existing untrusted copy of the target release")?;
        }
        fs::rename(&extracted, &final_root).with_context(|| {
            format!(
                "failed to promote staged release from {} to {}",
                extracted.display(),
                final_root.display()
            )
        })?;
        validate_package_layout(&final_root, &release.version, current_release_target()?)?;
        Ok(final_root)
    }

    async fn require_asset_attestation(&self, asset: &VerifiedAsset) -> Result<()> {
        let digest = sha256_digest_label(&asset.sha256);
        let url = format!(
            "https://api.github.com/repos/{GITHUB_REPOSITORY}/attestations/{digest}"
        );
        let response = self
            .inner
            .client
            .get(url)
            .send()
            .await
            .with_context(|| format!("failed to check the attestation for {}", asset.name))?;
        if !response.status().is_success() {
            bail!(
                "{} attestation check returned HTTP {}",
                asset.name,
                response.status().as_u16()
            );
        }
        let bytes = bounded_response(response, API_RESPONSE_LIMIT).await?;
        validate_attestation_response(&bytes)
            .with_context(|| format!("{} is not attested by the official repository", asset.name))
    }

    async fn download_asset(
        &self,
        asset: &VerifiedAsset,
        destination: &Path,
        report_progress: bool,
    ) -> Result<[u8; 32]> {
        let response = self
            .inner
            .client
            .get(asset.url.clone())
            .send()
            .await
            .with_context(|| format!("failed to download {}", asset.name))?;
        if !response.status().is_success() {
            bail!(
                "{} download returned HTTP {}",
                asset.name,
                response.status().as_u16()
            );
        }
        if response
            .content_length()
            .is_some_and(|length| length > asset.size || length > ARCHIVE_DOWNLOAD_LIMIT)
        {
            bail!("{} download length exceeds the expected size", asset.name);
        }

        let mut output = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(destination)
            .await
            .with_context(|| format!("failed to create {}", destination.display()))?;
        let mut stream = response.bytes_stream();
        let mut hasher = Sha256::new();
        let mut downloaded = 0_u64;
        let mut last_progress = 0_u8;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.with_context(|| format!("{} download failed", asset.name))?;
            downloaded = downloaded
                .checked_add(chunk.len() as u64)
                .context("download length overflow")?;
            if downloaded > asset.size || downloaded > ARCHIVE_DOWNLOAD_LIMIT {
                bail!("{} download exceeded the expected size", asset.name);
            }
            output
                .write_all(&chunk)
                .await
                .with_context(|| format!("failed to write {}", destination.display()))?;
            hasher.update(&chunk);
            if report_progress && asset.size > 0 {
                let progress = ((downloaded.saturating_mul(100) / asset.size).min(99)) as u8;
                if progress >= last_progress.saturating_add(5) {
                    last_progress = progress;
                    self.replace_status(UpdateState::Downloading, Some(progress), None)
                        .await;
                }
            }
        }
        output
            .flush()
            .await
            .with_context(|| format!("failed to flush {}", destination.display()))?;
        drop(output);
        if downloaded != asset.size {
            bail!(
                "{} download size {} does not match GitHub metadata {}",
                asset.name,
                downloaded,
                asset.size
            );
        }
        Ok(hasher.finalize().into())
    }

    async fn replace_status(
        &self,
        state: UpdateState,
        progress_percent: Option<u8>,
        error: Option<String>,
    ) {
        let mut status = self.inner.status.write().await;
        status.state = state;
        status.progress_percent = progress_percent;
        status.error = error;
    }

    async fn fail_status(&self, error: &anyhow::Error) {
        let mut message = format!("{error:#}");
        message.truncate(400);
        self.replace_status(UpdateState::Failed, None, Some(message))
            .await;
    }

    fn apply_install_support(&self, status: &mut UpdateStatus, release_immutable: bool) {
        if self.inner.policy == UpdatePolicy::Off {
            status.install_supported = false;
            status.install_reason =
                Some("Automatic update checks are disabled by the server operator.".to_owned());
        } else if let Err(reason) = &self.inner.package_support {
            status.install_supported = false;
            status.install_reason = Some(reason.clone());
        } else if !release_immutable && status.state == UpdateState::Available {
            status.install_supported = false;
            status.install_reason = Some(
                "This release is not immutable on GitHub, so it cannot be installed automatically."
                    .to_owned(),
            );
        } else {
            status.install_supported = true;
            status.install_reason = None;
        }
    }
}

fn github_headers() -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static("codex-web-terminal-updater"),
    );
    headers.insert(
        "x-github-api-version",
        HeaderValue::from_static("2026-03-10"),
    );
    Ok(headers)
}

fn redirect_policy(attempt: Attempt<'_>) -> reqwest::redirect::Action {
    if attempt.previous().len() >= MAX_REDIRECTS || !allowed_https_url(attempt.url()) {
        attempt.stop()
    } else {
        attempt.follow()
    }
}

fn allowed_https_url(url: &Url) -> bool {
    if url.scheme() != "https"
        || url.username() != ""
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    matches!(
        url.host_str(),
        Some(
            "api.github.com"
                | "github.com"
                | "objects.githubusercontent.com"
                | "release-assets.githubusercontent.com"
        )
    )
}

async fn bounded_response(response: reqwest::Response, limit: u64) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        bail!("HTTP response is larger than the allowed limit");
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("HTTP response body failed")?;
        if bytes.len().saturating_add(chunk.len()) > limit as usize {
            bail!("HTTP response exceeded the allowed limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn select_asset(
    assets: &[GitHubAsset],
    expected_name: &str,
    version: &Version,
    size_limit: u64,
) -> Result<VerifiedAsset> {
    let matches = assets
        .iter()
        .filter(|asset| asset.name == expected_name)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        bail!("release must contain exactly one {expected_name} asset");
    }
    let asset = matches[0];
    if asset.state != "uploaded" || asset.size == 0 || asset.size > size_limit {
        bail!("{expected_name} has an invalid upload state or size");
    }
    let url = Url::parse(&asset.browser_download_url)
        .with_context(|| format!("{expected_name} has an invalid URL"))?;
    validate_asset_url(&url, expected_name, version)?;
    let sha256 = parse_sha256_digest(
        asset
            .digest
            .as_deref()
            .context("GitHub asset metadata does not contain a digest")?,
    )?;
    Ok(VerifiedAsset {
        name: expected_name.to_owned(),
        url,
        size: asset.size,
        sha256,
    })
}

fn validate_asset_url(url: &Url, asset_name: &str, version: &Version) -> Result<()> {
    if !allowed_https_url(url) || url.host_str() != Some("github.com") || url.query().is_some() {
        bail!("{asset_name} URL is outside the official GitHub release location");
    }
    let expected_path = format!("/bproject07/Codex-web/releases/download/v{version}/{asset_name}");
    if url.path() != expected_path {
        bail!("{asset_name} URL does not match the checked release");
    }
    Ok(())
}

fn parse_sha256_digest(value: &str) -> Result<[u8; 32]> {
    let hexadecimal = value
        .strip_prefix("sha256:")
        .context("asset digest is not SHA-256")?;
    parse_sha256_hex(hexadecimal)
}

fn sha256_digest_label(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity("sha256:".len() + digest.len() * 2);
    output.push_str("sha256:");
    for byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn validate_attestation_response(bytes: &[u8]) -> Result<()> {
    let response: GitHubAttestationResponse =
        serde_json::from_slice(bytes).context("GitHub returned invalid attestation metadata")?;
    if !response
        .attestations
        .iter()
        .any(|attestation| attestation.repository_id == GITHUB_REPOSITORY_ID)
    {
        bail!("no matching repository attestation was found");
    }
    Ok(())
}

fn parse_sha256_hex(value: &str) -> Result<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid SHA-256 digest");
    }
    let mut output = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let text = std::str::from_utf8(pair).expect("ASCII hexadecimal");
        output[index] = u8::from_str_radix(text, 16).context("invalid SHA-256 digest")?;
    }
    Ok(output)
}

fn checksum_for_asset(bytes: &[u8], asset_name: &str) -> Result<[u8; 32]> {
    if bytes.len() > CHECKSUM_LIMIT as usize || bytes.contains(&0) {
        bail!("SHA256SUMS.txt is invalid or too large");
    }
    let text = std::str::from_utf8(bytes).context("SHA256SUMS.txt is not UTF-8")?;
    let mut found = None;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.split_whitespace();
        let digest = fields.next().context("invalid SHA256SUMS.txt line")?;
        let name = fields
            .next()
            .context("invalid SHA256SUMS.txt line")?
            .trim_start_matches('*');
        if fields.next().is_some() {
            bail!("invalid SHA256SUMS.txt line");
        }
        if name == asset_name {
            if found.is_some() {
                bail!("SHA256SUMS.txt lists the selected asset more than once");
            }
            found = Some(parse_sha256_hex(digest)?);
        }
    }
    found.context("SHA256SUMS.txt does not list the selected release archive")
}

fn ensure_updates_root(state_dir: &Path) -> Result<PathBuf> {
    if !state_dir.exists() {
        fs::create_dir_all(state_dir)
            .with_context(|| format!("failed to create state directory {}", state_dir.display()))?;
    }
    validate_regular_directory(state_dir)?;
    let updates_root = state_dir.join("updates");
    ensure_private_directory(&updates_root)?;
    ensure_private_directory(&updates_root.join("staging"))?;
    Ok(updates_root)
}

fn extract_release_archive(
    archive_path: &Path,
    extraction_root: &Path,
    version: &Version,
    target: &str,
) -> Result<PathBuf> {
    ensure_private_directory(extraction_root)?;
    let expected_root = expected_archive_root(version)?;
    if archive_path.extension().and_then(|value| value.to_str()) == Some("zip") {
        extract_zip(archive_path, extraction_root, &expected_root)?;
    } else {
        extract_tar_gz(archive_path, extraction_root, &expected_root)?;
    }
    let package_root = extraction_root.join(&expected_root);
    validate_package_layout(&package_root, version, target)?;
    validate_release_executable(&package_root, version)?;
    Ok(package_root)
}

fn extract_zip(archive_path: &Path, destination: &Path, expected_root: &str) -> Result<()> {
    let file = File::open(archive_path)
        .with_context(|| format!("failed to open {}", archive_path.display()))?;
    let mut archive = ZipArchive::new(file).context("release ZIP is invalid")?;
    if archive.is_empty() || archive.len() > ARCHIVE_ENTRY_LIMIT {
        bail!("release ZIP contains an invalid number of entries");
    }
    let mut names = HashSet::new();
    let mut total = 0_u64;

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .context("failed to read release ZIP entry")?;
        let name =
            std::str::from_utf8(entry.name_raw()).context("release ZIP path is not UTF-8")?;
        let is_directory = entry.is_dir();
        let relative = validate_archive_path(name, expected_root, is_directory, &mut names)?;
        if let Some(mode) = entry.unix_mode() {
            let kind = mode & 0o170000;
            if kind != 0 && kind != 0o040000 && kind != 0o100000 {
                bail!("release ZIP contains a link or special file");
            }
        }
        total = total
            .checked_add(entry.size())
            .context("release ZIP size overflow")?;
        if total > EXTRACTED_BYTES_LIMIT {
            bail!("release ZIP expands beyond the allowed limit");
        }
        if entry.compressed_size() > 0
            && entry.size() > 1024 * 1024
            && entry.size() / entry.compressed_size().max(1) > 1_000
        {
            bail!("release ZIP entry has an unsafe compression ratio");
        }
        let output = destination.join(relative);
        if is_directory {
            fs::create_dir_all(&output)
                .with_context(|| format!("failed to create {}", output.display()))?;
        } else {
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&output)
                .with_context(|| format!("failed to create {}", output.display()))?;
            let copied = io::copy(&mut entry, &mut file)
                .with_context(|| format!("failed to extract {}", output.display()))?;
            if copied != entry.size() {
                bail!("release ZIP entry changed size while extracting");
            }
            file.flush()
                .with_context(|| format!("failed to flush {}", output.display()))?;
        }
    }
    Ok(())
}

fn extract_tar_gz(archive_path: &Path, destination: &Path, expected_root: &str) -> Result<()> {
    let file = File::open(archive_path)
        .with_context(|| format!("failed to open {}", archive_path.display()))?;
    let decoder = GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let mut names = HashSet::new();
    let mut count = 0_usize;
    let mut total = 0_u64;
    for entry in archive
        .entries()
        .context("release tar archive is invalid")?
    {
        let mut entry = entry.context("failed to read release tar entry")?;
        count += 1;
        if count > ARCHIVE_ENTRY_LIMIT {
            bail!("release tar archive contains too many entries");
        }
        let entry_type = entry.header().entry_type();
        let is_directory = entry_type.is_dir();
        if !is_directory && !entry_type.is_file() {
            bail!("release tar archive contains a link or special file");
        }
        let path = entry.path().context("release tar path is invalid")?;
        let name = path
            .to_str()
            .context("release tar path is not valid UTF-8")?;
        let relative = validate_archive_path(name, expected_root, is_directory, &mut names)?;
        let size = entry.header().size().context("invalid tar entry size")?;
        total = total
            .checked_add(size)
            .context("release tar size overflow")?;
        if total > EXTRACTED_BYTES_LIMIT {
            bail!("release tar archive expands beyond the allowed limit");
        }
        let output = destination.join(relative);
        if is_directory {
            fs::create_dir_all(&output)
                .with_context(|| format!("failed to create {}", output.display()))?;
        } else {
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&output)
                .with_context(|| format!("failed to create {}", output.display()))?;
            let copied = io::copy(&mut entry, &mut file)
                .with_context(|| format!("failed to extract {}", output.display()))?;
            if copied != size {
                bail!("release tar entry changed size while extracting");
            }
            file.flush()
                .with_context(|| format!("failed to flush {}", output.display()))?;
        }
    }
    if count == 0 {
        bail!("release tar archive is empty");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let binary = destination.join(expected_root).join(executable_name());
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("failed to make {} executable", binary.display()))?;
    }
    Ok(())
}

fn validate_archive_path(
    name: &str,
    expected_root: &str,
    is_directory: bool,
    names: &mut HashSet<String>,
) -> Result<PathBuf> {
    if name.is_empty()
        || name.len() > ARCHIVE_PATH_LIMIT
        || name.contains('\\')
        || name.contains('\0')
        || name.starts_with('/')
        || name.chars().any(char::is_control)
    {
        bail!("release archive contains an unsafe path");
    }
    let normalized = if is_directory {
        name.trim_end_matches('/')
    } else {
        name
    };
    if normalized.is_empty() {
        bail!("release archive contains an empty path");
    }
    let path = Path::new(normalized);
    let mut components = path.components();
    match components.next() {
        Some(Component::Normal(root)) if root == expected_root => {}
        _ => bail!("release archive does not use the expected root directory"),
    }
    for component in path.components() {
        let Component::Normal(value) = component else {
            bail!("release archive path contains traversal or a prefix");
        };
        let text = value
            .to_str()
            .context("release archive path is not valid UTF-8")?;
        if text.is_empty()
            || text == "."
            || text == ".."
            || text.ends_with(['.', ' '])
            || text.contains(':')
            || is_windows_reserved_name(text)
        {
            bail!("release archive contains an unsafe path component");
        }
    }
    let collision_key = normalized.to_lowercase();
    if !names.insert(collision_key) {
        bail!("release archive contains duplicate or case-colliding paths");
    }
    Ok(path.to_path_buf())
}

fn is_windows_reserved_name(value: &str) -> bool {
    let stem = value
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let Some(suffix) = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
    else {
        return false;
    };
    matches!(
        suffix,
        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
    )
}

fn validate_executable_format(path: &Path) -> Result<()> {
    let mut file =
        File::open(path).with_context(|| format!("failed to inspect {}", path.display()))?;
    let mut header = vec![0_u8; 1024 * 1024];
    let read = file
        .read(&mut header)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    header.truncate(read);
    if cfg!(windows) {
        if header.len() < 0x40 || &header[..2] != b"MZ" {
            bail!("release executable is not a Windows PE image");
        }
        let offset =
            u32::from_le_bytes(header[0x3c..0x40].try_into().expect("four bytes")) as usize;
        if offset.checked_add(6).is_none_or(|end| end > header.len())
            || &header[offset..offset + 4] != b"PE\0\0"
            || u16::from_le_bytes(
                header[offset + 4..offset + 6]
                    .try_into()
                    .expect("two bytes"),
            ) != 0x8664
        {
            bail!("release executable is not an x86-64 PE image");
        }
    } else if header.len() < 20
        || &header[..4] != b"\x7fELF"
        || header[4] != 2
        || header[5] != 1
        || u16::from_le_bytes(header[18..20].try_into().expect("two bytes")) != 62
    {
        bail!("release executable is not an x86-64 little-endian ELF image");
    }
    Ok(())
}

fn validate_binary_version(path: &Path, expected: &Version) -> Result<()> {
    let mut command = Command::new(path);
    command.arg("--version");
    configure_staged_version_probe_environment(&mut command);
    let output = run_bounded(
        &mut command,
        BoundedProcessOptions {
            timeout: STAGED_VERSION_PROBE_TIMEOUT,
            stdout_limit: STAGED_VERSION_OUTPUT_LIMIT,
            stderr_limit: STAGED_VERSION_OUTPUT_LIMIT,
        },
    )
    .with_context(|| format!("failed to run {} --version", path.display()))?;
    if !output.status.success() || output.stdout_truncated || output.stderr_truncated {
        bail!("staged release executable failed its version check");
    }
    let reported =
        std::str::from_utf8(&output.stdout).context("staged executable version is not UTF-8")?;
    if reported.trim() != format!("codex-web {expected}") {
        bail!("staged executable reports an unexpected version");
    }
    Ok(())
}

pub fn validate_release_executable(package_root: &Path, expected: &Version) -> Result<()> {
    let executable = package_root.join(executable_name());
    validate_executable_format(&executable)?;
    validate_binary_version(&executable, expected)
}

fn configure_staged_version_probe_environment(command: &mut Command) {
    let configured_environment = command
        .get_envs()
        .map(|(name, value)| (name.to_owned(), value.map(OsString::from)))
        .collect::<Vec<_>>();
    let sanitized_environment = env::vars_os().filter(|(name, _)| !is_probe_secret(name));
    command.env_clear().envs(sanitized_environment);
    for (name, value) in configured_environment {
        if is_probe_secret(&name) {
            continue;
        }
        match value {
            Some(value) => {
                command.env(name, value);
            }
            None => {
                command.env_remove(name);
            }
        }
    }
}

fn is_probe_secret(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name.eq_ignore_ascii_case("CODEX_WEB_TOKEN")
        || name.eq_ignore_ascii_case(SUPERVISED_WORKER_ENV)
        || name.eq_ignore_ascii_case(READINESS_NONCE_ENV)
        || name.eq_ignore_ascii_case(SERVER_RESTART_CAPABILITY_ENV)
        || name.eq_ignore_ascii_case("CODEX_THREAD_ID")
        || name.eq_ignore_ascii_case("CLAUDECODE")
        || name
            .get(.."CWT_PEER_".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("CWT_PEER_"))
}

fn rfc3339_now() -> String {
    // Date conversion adapted from the public-domain civil calendar algorithm
    // by Howard Hinnant. Seconds are sufficient for update diagnostics.
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (seconds / 86_400) as i64;
    let day_seconds = seconds % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let hour = day_seconds / 3_600;
    let minute = day_seconds % 3_600 / 60;
    let second = day_seconds % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use zip::{ZipWriter, write::SimpleFileOptions};

    #[test]
    fn staged_version_probe_removes_agent_context_and_peer_secrets() {
        fn configured_value<'a>(command: &'a Command, name: &str) -> Option<&'a OsStr> {
            command
                .get_envs()
                .find(|(candidate, _)| candidate.to_string_lossy().eq_ignore_ascii_case(name))
                .and_then(|(_, value)| value)
        }

        let mut command = Command::new("unused");
        command
            .env("CWT_SAFE_FIXTURE", "preserved")
            .env("CODEX_WEB_TOKEN", "server-secret")
            .env(SUPERVISED_WORKER_ENV, "1")
            .env(READINESS_NONCE_ENV, "internal-nonce")
            .env(SERVER_RESTART_CAPABILITY_ENV, "1")
            .env("CODEX_THREAD_ID", "parent-codex")
            .env("CLAUDECODE", "parent-claude")
            .env("CWT_PEER_FUTURE_SECRET", "peer-secret");

        configure_staged_version_probe_environment(&mut command);

        assert_eq!(
            configured_value(&command, "CWT_SAFE_FIXTURE"),
            Some(OsStr::new("preserved"))
        );
        for secret in [
            "CODEX_WEB_TOKEN",
            SUPERVISED_WORKER_ENV,
            READINESS_NONCE_ENV,
            SERVER_RESTART_CAPABILITY_ENV,
            "CODEX_THREAD_ID",
            "CLAUDECODE",
            "CWT_PEER_FUTURE_SECRET",
        ] {
            assert_eq!(configured_value(&command, secret), None);
        }
    }

    #[test]
    fn parses_only_exact_sha256_digests() {
        assert_eq!(
            parse_sha256_digest(&format!("sha256:{}", "ab".repeat(32))).expect("valid digest"),
            [0xab; 32]
        );
        assert_eq!(
            sha256_digest_label(&[0xab; 32]),
            format!("sha256:{}", "ab".repeat(32))
        );
        assert!(parse_sha256_digest(&"ab".repeat(32)).is_err());
        assert!(parse_sha256_digest("sha256:not-a-hash").is_err());
    }

    #[test]
    fn attestation_metadata_requires_the_official_repository() {
        let valid = format!(
            r#"{{"attestations":[{{"repository_id":{GITHUB_REPOSITORY_ID}}}]}}"#
        );
        validate_attestation_response(valid.as_bytes()).expect("official attestation");
        assert!(validate_attestation_response(br#"{"attestations":[]}"#).is_err());
        assert!(
            validate_attestation_response(br#"{"attestations":[{"repository_id":7}]}"#)
                .is_err()
        );
    }

    #[test]
    fn checksum_file_requires_one_exact_asset_name() {
        let digest = "01".repeat(32);
        let text = format!(
            "{digest}  other.zip\n{digest}  codex-web-terminal-v1.2.3-windows-x86_64.zip\n"
        );
        assert_eq!(
            checksum_for_asset(
                text.as_bytes(),
                "codex-web-terminal-v1.2.3-windows-x86_64.zip"
            )
            .expect("selected checksum"),
            [1; 32]
        );
        assert!(checksum_for_asset(text.as_bytes(), "missing.zip").is_err());
    }

    #[test]
    fn archive_paths_reject_traversal_links_and_case_collisions() {
        let mut names = HashSet::new();
        validate_archive_path(
            "codex-web-terminal-v1.2.3-windows-x86_64/web/index.html",
            "codex-web-terminal-v1.2.3-windows-x86_64",
            false,
            &mut names,
        )
        .expect("safe path");
        assert!(
            validate_archive_path(
                "codex-web-terminal-v1.2.3-windows-x86_64/web/INDEX.HTML",
                "codex-web-terminal-v1.2.3-windows-x86_64",
                false,
                &mut names,
            )
            .is_err()
        );
        assert!(
            validate_archive_path(
                "codex-web-terminal-v1.2.3-windows-x86_64/../outside",
                "codex-web-terminal-v1.2.3-windows-x86_64",
                false,
                &mut HashSet::new(),
            )
            .is_err()
        );
        assert!(
            validate_archive_path(
                "/codex-web-terminal-v1.2.3-windows-x86_64/web/index.html",
                "codex-web-terminal-v1.2.3-windows-x86_64",
                false,
                &mut HashSet::new(),
            )
            .is_err()
        );
        for unsafe_name in [
            "codex-web-terminal-v1.2.3-windows-x86_64/web/COM¹.txt",
            "codex-web-terminal-v1.2.3-windows-x86_64/web/NUL.log",
            "codex-web-terminal-v1.2.3-windows-x86_64/web/asset:stream",
            "codex-web-terminal-v1.2.3-windows-x86_64/web/trailing.",
            "codex-web-terminal-v1.2.3-windows-x86_64/web/control\u{7f}",
        ] {
            assert!(
                validate_archive_path(
                    unsafe_name,
                    "codex-web-terminal-v1.2.3-windows-x86_64",
                    false,
                    &mut HashSet::new(),
                )
                .is_err(),
                "unsafe archive path was accepted: {unsafe_name:?}"
            );
        }
        let oversized = format!(
            "codex-web-terminal-v1.2.3-windows-x86_64/web/{}",
            "x".repeat(ARCHIVE_PATH_LIMIT)
        );
        assert!(
            validate_archive_path(
                &oversized,
                "codex-web-terminal-v1.2.3-windows-x86_64",
                false,
                &mut HashSet::new(),
            )
            .is_err()
        );
    }

    #[test]
    fn only_expected_github_asset_urls_are_accepted() {
        let version = Version::new(1, 2, 3);
        let name = "codex-web-terminal-v1.2.3-windows-x86_64.zip";
        let valid = Url::parse(&format!(
            "https://github.com/bproject07/Codex-web/releases/download/v{version}/{name}"
        ))
        .expect("valid URL");
        validate_asset_url(&valid, name, &version).expect("official URL");

        let wrong_host = Url::parse(&format!(
            "https://example.com/bproject07/Codex-web/releases/download/v{version}/{name}"
        ))
        .expect("valid URL");
        assert!(validate_asset_url(&wrong_host, name, &version).is_err());
    }

    #[test]
    fn timestamp_is_rfc3339_utc_shape() {
        let value = rfc3339_now();
        assert_eq!(value.len(), 20);
        assert_eq!(&value[4..5], "-");
        assert_eq!(&value[10..11], "T");
        assert!(value.ends_with('Z'));
    }

    #[test]
    fn zip_extraction_rejects_traversal_before_writing_outside() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let archive_path = temporary.path().join("unsafe.zip");
        let file = File::create(&archive_path).expect("archive file");
        let mut writer = ZipWriter::new(file);
        writer
            .start_file(
                "codex-web-terminal-v1.2.3-windows-x86_64/../escape.txt",
                SimpleFileOptions::default(),
            )
            .expect("unsafe fixture path");
        writer.write_all(b"escape").expect("fixture payload");
        writer.finish().expect("finish ZIP");
        let extraction = temporary.path().join("extracted");
        ensure_private_directory(&extraction).expect("extraction root");

        assert!(
            extract_zip(
                &archive_path,
                &extraction,
                "codex-web-terminal-v1.2.3-windows-x86_64",
            )
            .is_err()
        );
        assert!(!temporary.path().join("escape.txt").exists());
    }

    #[test]
    fn tar_extraction_rejects_symbolic_links() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let archive_path = temporary.path().join("unsafe.tar.gz");
        let encoder = GzEncoder::new(
            File::create(&archive_path).expect("archive file"),
            Compression::default(),
        );
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header
            .set_link_name("../../escape")
            .expect("fixture link target");
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                "codex-web-terminal-v1.2.3-linux-x86_64-glibc/web/link",
                io::empty(),
            )
            .expect("unsafe link fixture");
        builder.finish().expect("finish TAR");
        let extraction = temporary.path().join("extracted");
        ensure_private_directory(&extraction).expect("extraction root");

        assert!(
            extract_tar_gz(
                &archive_path,
                &extraction,
                "codex-web-terminal-v1.2.3-linux-x86_64-glibc",
            )
            .is_err()
        );
    }
}
