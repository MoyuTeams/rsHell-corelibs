mod error;
mod storage;

use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use rshell_core::{
    HostKeyDecision, HostKeyPrompt, InteractionId, InteractionRequest, InteractionResponse,
};
use rshell_platform::PlatformPaths;
use russh::keys::{HashAlg, PublicKey, known_hosts};
use tokio::sync::Mutex;

use crate::InteractionBroker;

pub use error::{HostKeyError, HostKeyStorageStep};

const DEFAULT_INTERACTION_TIMEOUT: Duration = Duration::from_secs(60);

/// Verifies SSH server keys against rsHell's application-owned known-hosts file.
///
/// Clones share the write lock, so an accepted prompt is always rechecked before it can mutate
/// the file. Waiting for a user response deliberately happens outside that lock.
#[derive(Clone)]
pub struct KnownHostsVerifier {
    path: PathBuf,
    timeout: Duration,
    changed_key_prompt: bool,
    write_lock: Arc<Mutex<()>>,
}

impl KnownHostsVerifier {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            timeout: DEFAULT_INTERACTION_TIMEOUT,
            changed_key_prompt: false,
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn for_platform(paths: &PlatformPaths) -> Self {
        Self::new(paths.known_hosts_path())
    }

    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Asks about a changed key instead of failing closed without a prompt.
    ///
    /// The prompt for a key that differs from the recorded one carries `changed: true`.
    /// Accepting it replaces every recorded key for that host and port; any other response
    /// still fails with [`HostKeyError::Changed`]. Off by default.
    pub const fn with_changed_key_prompt(mut self) -> Self {
        self.changed_key_prompt = true;
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn verify(
        &self,
        host: &str,
        port: u16,
        key: &PublicKey,
        interactions: &InteractionBroker,
    ) -> Result<(), HostKeyError> {
        if !valid_endpoint(host, port) {
            return Err(HostKeyError::InvalidEndpoint);
        }
        let changed = match self.is_known(host, port, key) {
            Ok(true) => return Ok(()),
            Ok(false) => None,
            Err(error @ HostKeyError::Changed { .. }) if self.changed_key_prompt => {
                Some((error, self.recorded_keys(host, port)?))
            }
            Err(error) => return Err(error),
        };

        let response = self
            .request_confirmation(host, port, key, changed.is_some(), interactions)
            .await?;
        match response {
            InteractionResponse::HostKey(HostKeyDecision::AcceptAndStore) => {
                self.store_after_acceptance(host, port, key, changed).await
            }
            InteractionResponse::HostKey(HostKeyDecision::Reject)
            | InteractionResponse::Cancel
            | InteractionResponse::Secret(_)
            | InteractionResponse::Answers(_) => Err(changed
                .map(|(error, _)| error)
                .unwrap_or_else(|| HostKeyError::rejected(host, port))),
        }
    }

    async fn request_confirmation(
        &self,
        host: &str,
        port: u16,
        key: &PublicKey,
        changed: bool,
        interactions: &InteractionBroker,
    ) -> Result<InteractionResponse, HostKeyError> {
        let prompt = HostKeyPrompt {
            id: InteractionId::new(),
            host: host.to_owned(),
            port,
            algorithm: key.algorithm().to_string(),
            sha256: key.fingerprint(HashAlg::Sha256).to_string(),
            changed,
        };
        match tokio::time::timeout(
            self.timeout,
            interactions.request(InteractionRequest::HostKey(prompt)),
        )
        .await
        {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => Err(HostKeyError::interaction(host, port)),
            Err(_) => Err(HostKeyError::timeout(host, port)),
        }
    }

    /// Rechecks the endpoint's recorded keys under the shared write lock before replacing them.
    /// Line numbers alone cannot identify the key state confirmed by a changed-key prompt.
    async fn store_after_acceptance(
        &self,
        host: &str,
        port: u16,
        key: &PublicKey,
        changed: Option<(HostKeyError, Vec<PublicKey>)>,
    ) -> Result<(), HostKeyError> {
        let _write_guard = self.write_lock.lock().await;
        match self.is_known(host, port, key) {
            Ok(true) => Ok(()),
            Ok(false) => match changed {
                None => storage::store(&self.path, host, port, key),
                Some((error, _)) => Err(error),
            },
            Err(HostKeyError::Changed { line, .. }) => match changed {
                Some((_, observed)) if observed == self.recorded_keys(host, port)? => {
                    storage::replace(&self.path, host, port, key)
                }
                _ => Err(HostKeyError::changed(host, port, line)),
            },
            Err(error) => Err(error),
        }
    }

    fn is_known(&self, host: &str, port: u16, key: &PublicKey) -> Result<bool, HostKeyError> {
        if self.path.exists() && !self.path.is_file() {
            return Err(HostKeyError::verification(host, port));
        }
        match known_hosts::check_known_hosts_path(host, port, key, &self.path) {
            Ok(known) => Ok(known),
            Err(russh::keys::Error::KeyChanged { line }) => {
                Err(HostKeyError::changed(host, port, line))
            }
            Err(_) => Err(HostKeyError::verification(host, port)),
        }
    }

    fn recorded_keys(&self, host: &str, port: u16) -> Result<Vec<PublicKey>, HostKeyError> {
        let keys = known_hosts::known_host_keys_path(host, port, &self.path)
            .map_err(|_| HostKeyError::verification(host, port))?
            .into_iter()
            .map(|(_, key)| key)
            .collect::<Vec<_>>();
        if keys.is_empty() {
            return Err(HostKeyError::verification(host, port));
        }
        Ok(keys)
    }
}

impl fmt::Debug for KnownHostsVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("KnownHostsVerifier([REDACTED])")
    }
}

fn valid_endpoint(host: &str, _port: u16) -> bool {
    !host.is_empty() && !host.chars().any(char::is_whitespace)
}
