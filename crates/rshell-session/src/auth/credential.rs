use rshell_core::ConnectionProfile;
use rshell_storage::CredentialVault;
use secrecy::SecretString;

use super::AuthPlanError;

pub(super) fn required_secret(
    profile: &ConnectionProfile,
    vault: &dyn CredentialVault,
) -> Result<SecretString, AuthPlanError> {
    let Some(reference) = profile
        .credential_ref
        .as_ref()
        .filter(|reference| !reference.0.trim().is_empty())
    else {
        return Err(AuthPlanError::MissingCredentialRef {
            host: profile.host.clone(),
            authentication: profile.authentication,
        });
    };
    vault
        .get(reference)
        .map_err(|vault| AuthPlanError::CredentialFault {
            host: profile.host.clone(),
            authentication: profile.authentication,
            vault,
        })?
        .ok_or_else(|| AuthPlanError::CredentialMissing {
            host: profile.host.clone(),
            authentication: profile.authentication,
        })
}

pub(super) fn optional_secret(
    profile: &ConnectionProfile,
    vault: &dyn CredentialVault,
) -> Result<Option<SecretString>, AuthPlanError> {
    let Some(reference) = profile.credential_ref.as_ref() else {
        return Ok(None);
    };
    if reference.0.trim().is_empty() {
        return Err(AuthPlanError::MissingCredentialRef {
            host: profile.host.clone(),
            authentication: profile.authentication,
        });
    }
    vault
        .get(reference)
        .map_err(|vault| AuthPlanError::CredentialFault {
            host: profile.host.clone(),
            authentication: profile.authentication,
            vault,
        })
        .and_then(|secret| {
            secret.ok_or_else(|| AuthPlanError::CredentialMissing {
                host: profile.host.clone(),
                authentication: profile.authentication,
            })
        })
        .map(Some)
}
