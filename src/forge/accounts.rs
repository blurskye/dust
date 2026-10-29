//! Accounts, the identity-before-save flow, and which credential sources actually exist (W-22).
//!
//! The contract: "Authentication uses an approved credential store, an explicitly session-only
//! entry, or a named environment variable; there is no plaintext fallback and no silent
//! downgrade. Identity is verified against the provider before an account is saved, and a rejected
//! credential reports the provider's actual response." and "Every account binds an explicit
//! provider, host, API base, account and repository, and every write names both account and
//! repository before sending."

use super::providers::{ForgeOutcome, error_outcome, identity_address_on, identity_login};
use super::transport::ForgeTransport;
use super::{
    AccountBinding, ApiOrigin, Credential, CredentialSource, ForgeRefusal, Identity, Provider,
    verify_identity,
};

/// A forge account, with every binding field explicit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForgeAccount {
    pub provider: Provider,
    pub host: String,
    pub api_base: ApiOrigin,
    pub account: String,
    pub repository: String,
}

impl ForgeAccount {
    /// The binding a write is planned against.
    #[must_use]
    pub fn binding(&self) -> AccountBinding {
        AccountBinding {
            provider: self.provider,
            host: self.host.clone(),
            api_base: self.api_base.clone(),
            account: self.account.clone(),
            repository: self.repository.clone(),
        }
    }
}

/// An account together with the credential the provider CONFIRMED it with. The secret is kept for
/// the session so a write can be planned, and it is never printed.
#[derive(Clone, Eq, PartialEq)]
pub struct ConnectedAccount {
    pub account: ForgeAccount,
    credential: Credential,
}

impl ConnectedAccount {
    #[must_use]
    pub fn credential(&self) -> &Credential {
        &self.credential
    }
}

impl std::fmt::Debug for ConnectedAccount {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectedAccount")
            .field("account", &self.account)
            .field("credential", &self.credential)
            .finish()
    }
}

/// Why an account could not be connected. A rejection carries the provider's OWN response, because
/// "a rejected credential reports the provider's actual response".
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectRefusal {
    /// The provider answered with one of the four outcomes.
    Provider(ForgeOutcome),
    /// The provider reported an identity the credential was not asked about.
    UnconfirmedIdentity,
    /// The transport or policy refused before or during transmission.
    Refused(ForgeRefusal),
}

/// What has been CONNECTED in this session. It is deliberately not a store: see `sources`.
#[derive(Default)]
pub struct ForgeAccounts {
    connected: Vec<ConnectedAccount>,
}

impl ForgeAccounts {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn connected(&self) -> &[ConnectedAccount] {
        &self.connected
    }

    #[must_use]
    pub fn find(&self, provider: Provider, account: &str) -> Option<&ConnectedAccount> {
        self.connected
            .iter()
            .find(|held| held.account.provider == provider && held.account.account == account)
    }

    /// Connect an account: ask the provider WHOSE credential this is, and save only if it says so.
    pub fn connect(
        &mut self,
        transport: &ForgeTransport,
        account: ForgeAccount,
        credential: Credential,
    ) -> Result<&ConnectedAccount, ConnectRefusal> {
        // The ACCOUNT's api base, so a self-hosted forge is asked about its own credential.
        let address = identity_address_on(&account.api_base, account.provider)
            .map_err(ConnectRefusal::Refused)?;
        let origin = account.api_base.clone();
        let planned = super::plan_write(
            &account.binding(),
            &super::WriteTarget {
                address: &address,
                account: Some(&account.account),
                repository: Some(&account.repository),
            },
            Some(&credential),
        )
        .map_err(ConnectRefusal::Refused)?;
        let response = transport
            .send(account.provider, "GET", &planned, &origin, &[])
            .map_err(ConnectRefusal::Refused)?;
        if !(200..300).contains(&response.status) {
            return Err(ConnectRefusal::Provider(error_outcome(
                response.status,
                &response.body,
            )));
        }
        let reported = identity_login(account.provider, &response.body)
            .ok_or_else(|| ConnectRefusal::Provider(ForgeOutcome::Unknown(
                "the provider did not state whose credential this is".to_owned(),
            )))?;
        verify_identity(
            &Identity {
                host: account.host.clone(),
                account: account.account.clone(),
            },
            &Identity {
                host: account.host.clone(),
                account: reported,
            },
        )
        .map_err(|_| ConnectRefusal::UnconfirmedIdentity)?;
        // ONLY here, after the provider has confirmed the identity, is anything saved.
        self.connected.push(ConnectedAccount { account, credential });
        Ok(self.connected.last().expect("just pushed"))
    }
}

/// Reads a secret by NAME. The boundary exists so tests never mutate the process environment and
/// so the foreground session reads no ambient state directly.
pub trait SecretReader {
    fn read(&self, name: &str) -> Option<String>;
}

/// The production reader: a named environment variable.
///
/// The architecture rules constrain specific directories (`editor/text`, `editor/state`, `files`,
/// `workspace`, ...) against `std::fs` and dependency direction; they do not forbid `std::env` for
/// this module, and the plugin-host rule in the same audit is about clearing the environment for
/// child processes rather than about reading it here.
pub struct EnvironmentSecrets;

impl SecretReader for EnvironmentSecrets {
    fn read(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

/// Where a credential came from, or why it did not.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceRefusal {
    /// The source exists by contract but has no backend here, and refusing beats substituting one.
    StoreUnavailable,
    /// The named variable is not set.
    MissingVariable(String),
    /// The name is empty, so it names nothing.
    Unnamed,
}

/// Resolve a credential from an approved source.
///
/// The sources that EXIST are the explicitly session-only entry and a named environment variable.
/// A persistent credential store does NOT exist: it would need a persistence-schema change and
/// filesystem authority, so asking for one is refused rather than quietly satisfied from another
/// source, which is exactly the "no silent downgrade" the contract demands.
pub fn resolve(
    source: CredentialSource,
    name: &str,
    session: Option<&Credential>,
    reader: &dyn SecretReader,
) -> Result<Credential, SourceRefusal> {
    if name.is_empty() {
        return Err(SourceRefusal::Unnamed);
    }
    match source {
        CredentialSource::Session => session
            .filter(|credential| credential.name() == name)
            .cloned()
            .ok_or_else(|| SourceRefusal::MissingVariable(format!("session entry {name}"))),
        CredentialSource::Environment => reader
            .read(name)
            .map(|secret| Credential::new(CredentialSource::Environment, name, secret))
            .ok_or_else(|| SourceRefusal::MissingVariable(name.to_owned())),
        CredentialSource::Store => Err(SourceRefusal::StoreUnavailable),
    }
}
