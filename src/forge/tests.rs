use super::*;

fn binding() -> AccountBinding {
    AccountBinding {
        provider: Provider::GitHub,
        host: "github.com".into(),
        api_base: ApiOrigin::parse("https://api.github.com").expect("api base"),
        account: "dust-dev".into(),
        repository: "dust-editor".into(),
    }
}

#[test]
fn plaintext_is_refused_rather_than_used_as_a_fallback() {
    // The contract: "one bounded HTTPS transport ... no plaintext fallback and no silent
    // downgrade." An implementation without the check would return an origin here.
    assert_eq!(
        ApiOrigin::parse("http://api.github.com"),
        Err(ForgeRefusal::InsecureScheme)
    );
    assert_eq!(
        ApiOrigin::parse("api.github.com"),
        Err(ForgeRefusal::UnreadableAddress("api.github.com".into()))
    );
}

#[test]
fn credentials_written_into_the_address_are_refused() {
    assert_eq!(
        ApiOrigin::parse("https://user:secret@api.github.com"),
        Err(ForgeRefusal::InlineCredential)
    );
}

#[test]
fn a_request_that_would_leave_the_api_origin_is_refused_before_transmission() {
    let api = ApiOrigin::parse("https://api.github.com").expect("api base");
    assert_eq!(
        api.contains("https://evil.example/repos"),
        Err(ForgeRefusal::OriginEscape)
    );
    // A different PORT is a different origin, not the same host.
    assert_eq!(
        api.contains("https://api.github.com:8443/repos"),
        Err(ForgeRefusal::OriginEscape)
    );
    // The origin's own address is contained, so the check is not simply refusing everything.
    assert!(api.contains("https://api.github.com/repos/dust-dev/dust-editor").is_ok());
}

#[test]
fn a_write_must_name_the_account_and_the_repository() {
    let binding = binding();
    let credential = Credential::new(CredentialSource::Store, "github", "token");
    assert_eq!(
        plan_write(
            &binding,
            &WriteTarget {
                address: "https://api.github.com/repos/dust-dev/dust-editor/pulls",
                account: None,
                repository: Some("dust-editor"),
            },
            Some(&credential),
        ),
        Err(ForgeRefusal::UnboundWrite)
    );
    assert_eq!(
        plan_write(
            &binding,
            &WriteTarget {
                address: "https://api.github.com/repos/dust-dev/dust-editor/pulls",
                account: Some("dust-dev"),
                repository: None,
            },
            Some(&credential),
        ),
        Err(ForgeRefusal::UnboundWrite)
    );
}

#[test]
fn a_write_to_another_account_or_repository_is_refused() {
    let binding = binding();
    let credential = Credential::new(CredentialSource::Store, "github", "token");
    assert_eq!(
        plan_write(
            &binding,
            &WriteTarget {
                address: "https://api.github.com/repos/dust-dev/OTHER/pulls",
                account: Some("dust-dev"),
                repository: Some("OTHER"),
            },
            Some(&credential),
        ),
        Err(ForgeRefusal::BindingMismatch)
    );
    assert_eq!(
        plan_write(
            &binding,
            &WriteTarget {
                address: "https://api.github.com/repos/SOMEONE/dust-editor/pulls",
                account: Some("SOMEONE"),
                repository: Some("dust-editor"),
            },
            Some(&credential),
        ),
        Err(ForgeRefusal::BindingMismatch)
    );
}

#[test]
fn a_bound_write_inside_the_origin_is_admitted_with_its_credential() {
    // The control: an implementation that refused everything would pass every refusal test above.
    let binding = binding();
    let credential = Credential::new(CredentialSource::Session, "github", "token");
    let planned = plan_write(
        &binding,
        &WriteTarget {
            address: "https://api.github.com/repos/dust-dev/dust-editor/pulls",
            account: Some("dust-dev"),
            repository: Some("dust-editor"),
        },
        Some(&credential),
    )
    .expect("a bound write inside the origin is admitted");
    assert_eq!(planned.credential.as_ref().map(Credential::name), Some("github"));
}

#[test]
fn a_redirect_to_another_host_never_carries_the_credential() {
    // The contract: "credentials are never forwarded across a host redirect". The assertion is
    // the ABSENCE of the credential, because a request that merely succeeds proves nothing.
    let from = ApiOrigin::parse("https://api.github.com").expect("api base");
    let credential = Credential::new(CredentialSource::Store, "github", "token");
    let planned = redirect(
        &from,
        "https://objects.example/artifact",
        Some(&credential),
        &["objects.example"],
        0,
    )
    .expect("an explicitly allowed host may be followed");
    assert_eq!(planned.credential, None, "the credential is DROPPED, not forwarded");
    assert_eq!(planned.address, "https://objects.example/artifact");
    // The control: a SAME-HOST redirect keeps the credential, so "always drop it" is also wrong.
    let same = redirect(
        &from,
        "https://api.github.com/repos/dust-dev/dust-editor/pulls",
        Some(&credential),
        &[],
        0,
    )
    .expect("a same-host redirect stays inside the origin");
    assert_eq!(
        same.credential.as_ref().map(Credential::name),
        Some("github"),
        "a same-host redirect keeps its credential"
    );
}

#[test]
fn a_redirect_to_a_host_that_was_not_named_is_refused() {
    let from = ApiOrigin::parse("https://api.github.com").expect("api base");
    assert_eq!(
        redirect(&from, "https://elsewhere.example/artifact", None, &["objects.example"], 0),
        Err(ForgeRefusal::RedirectNotAllowed("elsewhere.example".into()))
    );
}

#[test]
fn the_redirect_chain_is_bounded_and_has_no_plaintext_downgrade() {
    let from = ApiOrigin::parse("https://api.github.com").expect("api base");
    assert_eq!(
        redirect(&from, "https://objects.example/a", None, &["objects.example"], MAX_REDIRECTS),
        Err(ForgeRefusal::RedirectLimit)
    );
    assert!(
        redirect(&from, "https://objects.example/a", None, &["objects.example"], MAX_REDIRECTS - 1).is_ok(),
        "the last permitted redirect is still followed"
    );
    assert_eq!(
        redirect(&from, "http://objects.example/a", None, &["objects.example"], 0),
        Err(ForgeRefusal::InsecureScheme)
    );
}

#[test]
fn response_bodies_are_bounded_at_the_bound_itself() {
    let at_bound = "x".repeat(MAX_RESPONSE_BYTES);
    assert!(bound_response(&at_bound, MAX_RESPONSE_BYTES).is_ok());
    let over = "x".repeat(MAX_RESPONSE_BYTES + 1);
    assert_eq!(
        bound_response(&over, MAX_RESPONSE_BYTES),
        Err(ForgeRefusal::ResponseTooLarge)
    );
}

#[test]
fn secrets_are_redacted_from_messages_and_never_printed_with_the_credential() {
    let credential = Credential::new(CredentialSource::Store, "github", "ghp_supersecret");
    let printed = format!("{credential:?}");
    assert!(
        !printed.contains("ghp_supersecret"),
        "the credential's Debug must not print its secret: {printed}"
    );
    assert!(printed.contains("<redacted>"));
    let message = redact(
        "provider rejected ghp_supersecret for dust-dev",
        &[credential.expose()],
    );
    assert!(!message.contains("ghp_supersecret"), "error text is redacted: {message}");
    assert!(message.contains("<redacted>"));
    assert!(message.contains("dust-dev"), "the non-secret context survives");
}

#[test]
fn an_identity_the_provider_does_not_confirm_is_refused_before_the_account_is_saved() {
    let asked = Identity {
        host: "github.com".into(),
        account: "dust-dev".into(),
    };
    let confirmed = asked.clone();
    assert!(verify_identity(&asked, &confirmed).is_ok());
    assert_eq!(
        verify_identity(
            &asked,
            &Identity {
                host: "github.com".into(),
                account: "someone-else".into(),
            }
        ),
        Err(ForgeRefusal::IdentityMismatch)
    );
    assert_eq!(
        verify_identity(
            &asked,
            &Identity {
                host: "gitlab.com".into(),
                account: "dust-dev".into(),
            }
        ),
        Err(ForgeRefusal::IdentityMismatch),
        "the HOST the provider reports is part of the identity"
    );
}
