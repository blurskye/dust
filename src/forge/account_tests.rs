//! Accounts, the identity-before-save flow and the credential sources that exist. Local fixtures
//! only: nothing here reaches a network.

use std::collections::BTreeMap;
use std::time::Duration;

use super::accounts::{
    ConnectRefusal, EnvironmentSecrets, ForgeAccount, ForgeAccounts, SecretReader, SourceRefusal,
    resolve,
};
use super::providers::{ForgeOperation, ForgeOutcome, identity_address, identity_login};
use super::transport::ForgeTransport;
use super::transport_tests::{Fixture, response};
use super::*;

/// A reader a test controls, so no test mutates the process environment.
struct MapSecrets(BTreeMap<String, String>);

impl SecretReader for MapSecrets {
    fn read(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }
}

fn transport() -> ForgeTransport {
    ForgeTransport::with_limits(
        Duration::from_millis(500),
        Duration::from_millis(1500),
        MAX_REDIRECTS,
    )
    .expect("transport")
}

fn account_for(fixture: &Fixture) -> ForgeAccount {
    ForgeAccount {
        provider: Provider::GitHub,
        host: fixture.origin.host().to_owned(),
        api_base: fixture.origin.clone(),
        account: "dust-dev".into(),
        repository: "dust-editor".into(),
    }
}

fn credential() -> Credential {
    Credential::new(CredentialSource::Session, "github", "ghp_supersecret")
}

#[test]
fn a_rejected_credential_reports_the_provider_response_and_saves_nothing() {
    let fixture = Fixture::serve(1, |_, _| {
        response("401 Unauthorized", &[], "Bad credentials")
    });
    let mut accounts = ForgeAccounts::new();
    let refused = accounts
        .connect(&transport(), account_for(&fixture), credential())
        .expect_err("a rejected credential connects nothing");
    assert_eq!(
        refused,
        ConnectRefusal::Provider(ForgeOutcome::Denied("Bad credentials".into())),
        "the provider's ACTUAL response is reported"
    );
    assert!(
        accounts.connected().is_empty(),
        "nothing is saved when the provider rejects the credential"
    );
    // And the credential really was SENT: the provider had something to reject.
    assert!(
        fixture.requests()[0].contains("ghp_supersecret"),
        "the credential was transmitted for verification: {:?}",
        fixture.requests()[0]
    );
}

#[test]
fn an_identity_the_provider_does_not_confirm_is_refused_and_nothing_is_saved() {
    let fixture = Fixture::serve(1, |_, _| {
        response("200 OK", &[("content-type", "application/json")], r#"{"login":"someone-else"}"#)
    });
    let mut accounts = ForgeAccounts::new();
    assert_eq!(
        accounts
            .connect(&transport(), account_for(&fixture), credential())
            .expect_err("an unconfirmed identity is refused"),
        ConnectRefusal::UnconfirmedIdentity
    );
    assert!(
        accounts.connected().is_empty(),
        "an account is only saved once the provider CONFIRMED the identity"
    );
}

#[test]
fn a_confirmed_identity_saves_every_binding_field_and_a_write_names_both() {
    let fixture = Fixture::serve(1, |_, _| {
        response("200 OK", &[("content-type", "application/json")], r#"{"login":"dust-dev"}"#)
    });
    let mut accounts = ForgeAccounts::new();
    accounts
        .connect(&transport(), account_for(&fixture), credential())
        .expect("a confirmed identity is saved");
    let saved = &accounts.connected()[0];
    assert_eq!(saved.account.provider, Provider::GitHub);
    assert_eq!(saved.account.host, fixture.origin.host());
    assert_eq!(saved.account.api_base, fixture.origin);
    assert_eq!(saved.account.account, "dust-dev");
    assert_eq!(saved.account.repository, "dust-editor");

    // A write planned from the saved account NAMES both, and a mismatched one is refused.
    // Built on the ACCOUNT's own api base, which is what a self-hosted forge needs.
    let address = super::providers::address_on(
        &saved.account.api_base,
        Provider::GitHub,
        ForgeOperation::ReviewComment,
        "dust-dev",
        "dust-editor",
        super::providers::Reference::Number(4),
    )
    .expect("comment address");
    let binding = saved.account.binding();
    assert!(
        plan_write(
            &binding,
            &WriteTarget {
                address: &address,
                account: Some("dust-dev"),
                repository: Some("dust-editor"),
            },
            Some(saved.credential()),
        )
        .is_ok(),
        "a write from the saved account names both and is admitted"
    );
    assert_eq!(
        plan_write(
            &binding,
            &WriteTarget {
                address: &address,
                account: Some("dust-dev"),
                repository: Some("another-repo"),
            },
            Some(saved.credential()),
        ),
        Err(ForgeRefusal::BindingMismatch),
        "a write to something other than the bound repository is refused"
    );
}

#[test]
fn identity_addresses_and_login_fields_are_each_provider_s_own() {
    let addresses: Vec<String> = [
        Provider::GitHub,
        Provider::GitLab,
        Provider::Gitea,
        Provider::Forgejo,
    ]
    .into_iter()
    .map(|provider| identity_address(provider).expect("every provider states an identity"))
    .collect();
    for (index, first) in addresses.iter().enumerate() {
        for second in addresses.iter().skip(index + 1) {
            assert_ne!(first, second, "two providers asked the same identity address");
        }
    }
    assert!(addresses[0].ends_with("/user"));
    assert!(addresses[1].contains("/api/v4/user"));
    assert!(addresses[2].contains("/api/v1/user"));
    assert!(addresses[3].contains("/api/v1/user"));

    // Each provider reports the login in its OWN field: a shared guess would read nothing and look
    // like a confirmed identity.
    assert_eq!(identity_login(Provider::GitHub, r#"{"login":"x"}"#), Some("x".into()));
    assert_eq!(identity_login(Provider::GitLab, r#"{"username":"x"}"#), Some("x".into()));
    assert_eq!(
        identity_login(Provider::GitLab, r#"{"login":"x"}"#),
        None,
        "GitLab does not report a `login` field"
    );
}

#[test]
fn the_sources_that_exist_resolve_and_the_one_that_does_not_is_refused() {
    let session = credential();
    let reader = MapSecrets(BTreeMap::from([(
        "FORGE_TOKEN".to_owned(),
        "env-secret".to_owned(),
    )]));

    let resolved = resolve(CredentialSource::Session, "github", Some(&session), &reader)
        .expect("a session entry resolves from what the session holds");
    assert_eq!(resolved.source(), CredentialSource::Session);
    assert_eq!(resolved.expose(), "ghp_supersecret");

    let resolved = resolve(CredentialSource::Environment, "FORGE_TOKEN", None, &reader)
        .expect("a named environment variable resolves through the boundary");
    assert_eq!(resolved.source(), CredentialSource::Environment);
    assert_eq!(resolved.expose(), "env-secret");

    assert_eq!(
        resolve(CredentialSource::Environment, "ABSENT", None, &reader),
        Err(SourceRefusal::MissingVariable("ABSENT".into())),
        "a variable that is not set resolves to nothing rather than to a fallback"
    );
    // The gap, refused rather than quietly satisfied from another source.
    assert_eq!(
        resolve(CredentialSource::Store, "keyring", None, &reader),
        Err(SourceRefusal::StoreUnavailable),
        "no persistent credential store exists, so asking for one is refused"
    );
    assert_eq!(
        resolve(CredentialSource::Session, "", Some(&session), &reader),
        Err(SourceRefusal::Unnamed)
    );
    // And the production reader exists without being exercised against ambient state.
    let _ = EnvironmentSecrets;
}
