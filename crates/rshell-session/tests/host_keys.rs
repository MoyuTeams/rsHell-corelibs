use std::{fs, time::Duration};

use rshell_core::{
    HostKeyDecision, HostKeyPrompt, InteractionRequest, InteractionResponse, SessionFailure,
};
use rshell_platform::private_file_is_secure;
use rshell_session::{HostKeyError, KnownHostsVerifier, interaction_channel};
use russh::keys::{HashAlg, PublicKey, parse_public_key_base64};
use tempfile::TempDir;

const KEY_A: &str = "AAAAC3NzaC1lZDI1NTE5AAAAILagOJFgwaMNhBWQINinKOXmqS4Gh5NgxgriXwdOoINJ";
const KEY_B: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ";
// The public half of the separate Ed25519 client key fixture in tests/support/ssh_server.rs.
const KEY_C: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIHeLC1lWiCYrXsf/85O/pkbUFZ6OGIt49PX3nw8iRoXE";

fn key_a() -> PublicKey {
    parse_public_key_base64(KEY_A).expect("valid ed25519 test key")
}

fn key_b() -> PublicKey {
    parse_public_key_base64(KEY_B).expect("valid distinct ed25519 test key")
}

fn key_c() -> PublicKey {
    parse_public_key_base64(KEY_C).expect("valid third ed25519 test key")
}

fn verifier(temp: &TempDir) -> KnownHostsVerifier {
    KnownHostsVerifier::new(temp.path().join("config").join("known_hosts"))
        .with_timeout(Duration::from_millis(50))
}

fn assert_no_known_hosts_temporary_files(verifier: &KnownHostsVerifier) {
    let leftovers = fs::read_dir(verifier.path().parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .filter(|name| name.to_string_lossy().starts_with(".rshell-known-hosts-"))
        .collect::<Vec<_>>();
    assert!(leftovers.is_empty(), "temporary known-hosts files leaked");
}

async fn accept_unknown(verifier: &KnownHostsVerifier, key: &PublicKey, host: &str, port: u16) {
    let (broker, mut requests) = interaction_channel();
    let verification = verifier.verify(host, port, key, &broker);
    tokio::pin!(verification);
    let request = tokio::select! {
        request = requests.recv() => request.expect("host-key request"),
        result = &mut verification => panic!("unexpected verification result: {result:?}"),
    };
    let InteractionRequest::HostKey(prompt) = request.1 else {
        panic!("expected host-key prompt");
    };
    broker
        .respond(
            prompt.id,
            InteractionResponse::HostKey(HostKeyDecision::AcceptAndStore),
        )
        .expect("host-key response accepted");
    verification.await.expect("unknown key accepted and stored");
}

#[tokio::test]
async fn known_key_completes_without_a_prompt() {
    let temp = TempDir::new().unwrap();
    let verifier = verifier(&temp);
    let key = key_a();
    accept_unknown(&verifier, &key, "known.test", 22).await;

    let (broker, mut requests) = interaction_channel();
    verifier
        .verify("known.test", 22, &key, &broker)
        .await
        .expect("stored key is known");
    assert!(
        tokio::time::timeout(Duration::from_millis(20), requests.recv())
            .await
            .is_err(),
        "known keys must not prompt"
    );
}

#[tokio::test]
async fn unknown_key_requires_algorithm_and_sha256_confirmation_before_learning() {
    let temp = TempDir::new().unwrap();
    let verifier = verifier(&temp);
    let stored_key = key_a();
    accept_unknown(&verifier, &stored_key, "preserved.test", 22).await;
    let before = fs::read(verifier.path()).unwrap();

    let key = key_b();
    let (broker, mut requests) = interaction_channel();
    let verification = verifier.verify("unknown.test", 2222, &key, &broker);
    tokio::pin!(verification);
    let request = tokio::select! {
        request = requests.recv() => request.expect("host-key request"),
        result = &mut verification => panic!("unexpected verification result: {result:?}"),
    };
    let InteractionRequest::HostKey(prompt) = request.1 else {
        panic!("expected host-key prompt");
    };
    assert_eq!(prompt.host, "unknown.test");
    assert_eq!(prompt.port, 2222);
    assert_eq!(prompt.algorithm, "ssh-ed25519");
    assert!(prompt.sha256.starts_with("SHA256:"));
    assert!(!prompt.changed);
    assert_eq!(fs::read(verifier.path()).unwrap(), before);

    broker
        .respond(
            prompt.id,
            InteractionResponse::HostKey(HostKeyDecision::AcceptAndStore),
        )
        .expect("host-key response accepted");
    verification.await.expect("accepted unknown key stored");

    assert!(fs::read(verifier.path()).unwrap().starts_with(&before));
    assert!(private_file_is_secure(verifier.path()).unwrap());
    assert_no_known_hosts_temporary_files(&verifier);
    let (broker, _requests) = interaction_channel();
    verifier
        .verify("unknown.test", 2222, &key, &broker)
        .await
        .expect("accepted key is now known");
}

#[tokio::test]
async fn rejected_unknown_key_leaves_live_file_unchanged() {
    let temp = TempDir::new().unwrap();
    let verifier = verifier(&temp);
    let existing = key_a();
    accept_unknown(&verifier, &existing, "preserved.test", 22).await;
    let before = fs::read(verifier.path()).unwrap();

    let key = key_b();
    let (broker, mut requests) = interaction_channel();
    let verification = verifier.verify("rejected.test", 22, &key, &broker);
    tokio::pin!(verification);
    let request = tokio::select! {
        request = requests.recv() => request.expect("host-key request"),
        result = &mut verification => panic!("unexpected verification result: {result:?}"),
    };
    let InteractionRequest::HostKey(prompt) = request.1 else {
        panic!("expected host-key prompt");
    };
    broker
        .respond(
            prompt.id,
            InteractionResponse::HostKey(HostKeyDecision::Reject),
        )
        .unwrap();
    assert!(matches!(
        verification.await,
        Err(HostKeyError::Rejected { .. })
    ));
    assert_eq!(fs::read(verifier.path()).unwrap(), before);
}

#[tokio::test(start_paused = true)]
async fn unanswered_unknown_key_times_out_and_cleans_the_pending_response() {
    let temp = TempDir::new().unwrap();
    let verifier = verifier(&temp);
    let key = key_a();
    let (broker, mut requests) = interaction_channel();
    let verification = verifier.verify("timeout.test", 22, &key, &broker);
    tokio::pin!(verification);
    let request = tokio::select! {
        request = requests.recv() => request.expect("host-key request"),
        result = &mut verification => panic!("unexpected verification result: {result:?}"),
    };
    let InteractionRequest::HostKey(prompt) = request.1 else {
        panic!("expected host-key prompt");
    };
    tokio::time::advance(Duration::from_millis(51)).await;
    assert!(matches!(
        verification.await,
        Err(HostKeyError::Timeout { .. })
    ));
    assert_eq!(
        broker
            .respond(
                prompt.id,
                InteractionResponse::HostKey(HostKeyDecision::AcceptAndStore)
            )
            .expect_err("expired response must not remain pending")
            .failure(),
        SessionFailure::Validation
    );
    assert!(!verifier.path().exists());
}

#[tokio::test]
async fn changed_key_fails_closed_without_prompt_or_live_mutation() {
    let temp = TempDir::new().unwrap();
    let verifier = verifier(&temp);
    let original = key_a();
    accept_unknown(&verifier, &original, "changed.test", 22).await;
    let before = fs::read(verifier.path()).unwrap();

    let (broker, mut requests) = interaction_channel();
    let error = verifier
        .verify("changed.test", 22, &key_b(), &broker)
        .await
        .expect_err("changed key must be rejected before interaction");
    assert!(matches!(&error, HostKeyError::Changed { .. }));
    assert_eq!(error.failure(), SessionFailure::HostKeyChanged);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), requests.recv())
            .await
            .is_err(),
        "changed keys must never have an acceptance path"
    );
    assert_eq!(fs::read(verifier.path()).unwrap(), before);
    for output in [format!("{error:?}"), error.to_string()] {
        assert!(!output.contains(KEY_A));
        assert!(!output.contains(KEY_B));
    }
}

#[tokio::test]
async fn zero_port_host_key_decision_is_retained_without_default_port_rewriting() {
    let temp = TempDir::new().unwrap();
    let verifier = verifier(&temp);
    let key = key_a();
    accept_unknown(&verifier, &key, "zero-port.test", 0).await;

    let contents = fs::read_to_string(verifier.path()).unwrap();
    assert!(contents.contains("[zero-port.test]:0 "));

    let (broker, mut requests) = interaction_channel();
    verifier
        .verify("zero-port.test", 0, &key, &broker)
        .await
        .expect("accepted zero-port decision must remain known");
    assert!(
        tokio::time::timeout(Duration::from_millis(20), requests.recv())
            .await
            .is_err(),
        "retained zero-port decisions must not prompt again"
    );
}

#[tokio::test]
async fn non_file_known_hosts_destination_fails_verification_without_prompt_or_mutation() {
    let temp = TempDir::new().unwrap();
    let verifier = verifier(&temp);
    fs::create_dir_all(verifier.path()).unwrap();
    let (broker, mut requests) = interaction_channel();
    let key = key_a();
    assert!(matches!(
        verifier
            .verify("storage-failure.test", 22, &key, &broker)
            .await,
        Err(HostKeyError::Verification { .. })
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), requests.recv())
            .await
            .is_err(),
        "invalid known-hosts storage must fail before prompting"
    );
    assert!(verifier.path().is_dir());
    let entries = fs::read_dir(verifier.path().parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(entries, ["known_hosts"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_accepts_for_different_hosts_preserve_both_known_host_entries() {
    let temp = TempDir::new().unwrap();
    let verifier = verifier(&temp);
    let first_key = key_a();
    let second_key = key_b();
    let (broker_a, mut requests_a) = interaction_channel();
    let (broker_b, mut requests_b) = interaction_channel();
    let verifier_a = verifier.clone();
    let verifier_b = verifier.clone();
    let request_broker_a = broker_a.clone();
    let request_broker_b = broker_b.clone();

    let task_a = tokio::spawn(async move {
        verifier_a
            .verify("first.test", 22, &first_key, &request_broker_a)
            .await
    });
    let task_b = tokio::spawn(async move {
        verifier_b
            .verify("second.test", 22, &second_key, &request_broker_b)
            .await
    });
    let InteractionRequest::HostKey(prompt_a) = requests_a.recv().await.unwrap().1 else {
        panic!("expected first host-key prompt");
    };
    let InteractionRequest::HostKey(prompt_b) = requests_b.recv().await.unwrap().1 else {
        panic!("expected second host-key prompt");
    };
    broker_a
        .respond(
            prompt_a.id,
            InteractionResponse::HostKey(HostKeyDecision::AcceptAndStore),
        )
        .unwrap();
    broker_b
        .respond(
            prompt_b.id,
            InteractionResponse::HostKey(HostKeyDecision::AcceptAndStore),
        )
        .unwrap();

    task_a.await.unwrap().unwrap();
    task_b.await.unwrap().unwrap();
    let (broker, _requests) = interaction_channel();
    verifier
        .verify("first.test", 22, &key_a(), &broker)
        .await
        .unwrap();
    verifier
        .verify("second.test", 22, &key_b(), &broker)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_accepts_for_the_same_host_trust_exactly_one_key() {
    let temp = TempDir::new().unwrap();
    let verifier = verifier(&temp);
    let first_key = key_a();
    let second_key = key_b();
    let (broker_a, mut requests_a) = interaction_channel();
    let (broker_b, mut requests_b) = interaction_channel();
    let verifier_a = verifier.clone();
    let verifier_b = verifier.clone();
    let request_broker_a = broker_a.clone();
    let request_broker_b = broker_b.clone();

    let task_a = tokio::spawn(async move {
        verifier_a
            .verify("same.test", 22, &first_key, &request_broker_a)
            .await
    });
    let task_b = tokio::spawn(async move {
        verifier_b
            .verify("same.test", 22, &second_key, &request_broker_b)
            .await
    });
    let InteractionRequest::HostKey(prompt_a) = requests_a.recv().await.unwrap().1 else {
        panic!("expected first host-key prompt");
    };
    let InteractionRequest::HostKey(prompt_b) = requests_b.recv().await.unwrap().1 else {
        panic!("expected second host-key prompt");
    };
    broker_a
        .respond(
            prompt_a.id,
            InteractionResponse::HostKey(HostKeyDecision::AcceptAndStore),
        )
        .unwrap();
    broker_b
        .respond(
            prompt_b.id,
            InteractionResponse::HostKey(HostKeyDecision::AcceptAndStore),
        )
        .unwrap();

    let result_a = task_a.await.unwrap();
    let result_b = task_b.await.unwrap();
    assert!(
        matches!(
            (&result_a, &result_b),
            (Ok(()), Err(HostKeyError::Changed { .. }))
                | (Err(HostKeyError::Changed { .. }), Ok(()))
        ),
        "same host must not accept two distinct keys"
    );
    let (known_key, changed_key) = if result_a.is_ok() {
        (key_a(), key_b())
    } else {
        (key_b(), key_a())
    };
    let (broker, mut requests) = interaction_channel();
    verifier
        .verify("same.test", 22, &known_key, &broker)
        .await
        .unwrap();
    assert!(matches!(
        verifier
            .verify("same.test", 22, &changed_key, &broker)
            .await,
        Err(HostKeyError::Changed { .. })
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), requests.recv())
            .await
            .is_err(),
        "the changed key must not receive a new acceptance prompt"
    );
}

/// Verifies `key`, answers its host-key prompt with `decision` and returns the prompt together
/// with the verification result.
async fn answer_prompt(
    verifier: &KnownHostsVerifier,
    key: &PublicKey,
    host: &str,
    port: u16,
    decision: HostKeyDecision,
) -> (HostKeyPrompt, Result<(), HostKeyError>) {
    let (broker, mut requests) = interaction_channel();
    let verification = verifier.verify(host, port, key, &broker);
    tokio::pin!(verification);
    let request = tokio::select! {
        request = requests.recv() => request.expect("host-key request"),
        result = &mut verification => panic!("unexpected verification result: {result:?}"),
    };
    let InteractionRequest::HostKey(prompt) = request.1 else {
        panic!("expected host-key prompt");
    };
    broker
        .respond(prompt.id, InteractionResponse::HostKey(decision))
        .expect("host-key response accepted");
    (prompt, verification.await)
}

async fn assert_known(verifier: &KnownHostsVerifier, key: &PublicKey, host: &str, port: u16) {
    let (broker, mut requests) = interaction_channel();
    verifier
        .verify(host, port, key, &broker)
        .await
        .expect("recorded key is known");
    assert!(
        tokio::time::timeout(Duration::from_millis(20), requests.recv())
            .await
            .is_err(),
        "known keys must not prompt"
    );
}

#[tokio::test]
async fn opted_in_changed_key_prompt_warns_and_rejection_keeps_the_file() {
    let temp = TempDir::new().unwrap();
    let verifier = verifier(&temp).with_changed_key_prompt();
    accept_unknown(&verifier, &key_a(), "changed.test", 22).await;
    let before = fs::read(verifier.path()).unwrap();

    let (prompt, result) = answer_prompt(
        &verifier,
        &key_b(),
        "changed.test",
        22,
        HostKeyDecision::Reject,
    )
    .await;
    assert!(prompt.changed);
    assert_eq!(
        prompt.sha256,
        key_b().fingerprint(HashAlg::Sha256).to_string()
    );
    let error = result.expect_err("a rejected changed key must fail");
    assert!(matches!(&error, HostKeyError::Changed { .. }));
    assert_eq!(error.failure(), SessionFailure::HostKeyChanged);
    assert_eq!(fs::read(verifier.path()).unwrap(), before);
}

#[tokio::test]
async fn accepting_a_changed_key_replaces_only_that_hosts_entries() {
    let temp = TempDir::new().unwrap();
    let verifier = verifier(&temp).with_changed_key_prompt();
    accept_unknown(&verifier, &key_a(), "other.test", 22).await;
    accept_unknown(&verifier, &key_a(), "changed.test", 22).await;
    accept_unknown(&verifier, &key_a(), "changed.test", 2222).await;
    // A comment must neither be dropped nor shift which entries are replaced.
    let recorded = fs::read_to_string(verifier.path()).unwrap();
    fs::write(verifier.path(), format!("# kept comment\n{recorded}")).unwrap();

    let (prompt, result) = answer_prompt(
        &verifier,
        &key_b(),
        "changed.test",
        22,
        HostKeyDecision::AcceptAndStore,
    )
    .await;
    assert!(prompt.changed);
    result.expect("an accepted changed key replaces the recorded one");

    assert_known(&verifier, &key_b(), "changed.test", 22).await;
    assert_known(&verifier, &key_a(), "other.test", 22).await;
    assert_known(&verifier, &key_a(), "changed.test", 2222).await;
    let content = fs::read_to_string(verifier.path()).unwrap();
    assert!(content.starts_with("# kept comment\n"));
    assert_eq!(content.matches(KEY_A).count(), 2);
    assert_eq!(content.matches(KEY_B).count(), 1);
    assert!(private_file_is_secure(verifier.path()).unwrap());
    assert_no_known_hosts_temporary_files(&verifier);
}

#[tokio::test]
async fn accepting_a_changed_key_on_a_shared_line_preserves_the_other_endpoint() {
    let temp = TempDir::new().unwrap();
    let verifier = verifier(&temp).with_changed_key_prompt();
    // One shared record pins both endpoints to the same key, as ssh-keygen writes them.
    let shared = format!(
        "changed.test,other.test ssh-ed25519 {KEY_A}\n[changed.test]:2222 ssh-ed25519 {KEY_A}\n"
    );
    fs::create_dir_all(verifier.path().parent().unwrap()).unwrap();
    fs::write(verifier.path(), shared).unwrap();

    let (prompt, result) = answer_prompt(
        &verifier,
        &key_b(),
        "changed.test",
        22,
        HostKeyDecision::AcceptAndStore,
    )
    .await;
    assert!(prompt.changed);
    result.expect("an accepted changed key replaces only the confirmed endpoint");

    assert_known(&verifier, &key_b(), "changed.test", 22).await;
    assert_known(&verifier, &key_a(), "other.test", 22).await;
    assert_known(&verifier, &key_a(), "changed.test", 2222).await;
    let content = fs::read_to_string(verifier.path()).unwrap();
    assert!(
        content.contains(&format!("other.test ssh-ed25519 {KEY_A}")),
        "the shared line must keep the other endpoint's pin: {content}"
    );
    assert!(content.contains("changed.test "));
    assert!(private_file_is_secure(verifier.path()).unwrap());
    assert_no_known_hosts_temporary_files(&verifier);
}

#[tokio::test]
async fn replacing_a_shared_crlf_record_preserves_other_lines_and_their_endings() {
    let temp = TempDir::new().unwrap();
    let verifier = verifier(&temp).with_changed_key_prompt();
    let before = format!(
        "# retained comment\r\nchanged.test,other.test ssh-ed25519 {KEY_A}\r\n[changed.test]:2222 ssh-ed25519 {KEY_A}\r\n"
    );
    fs::create_dir_all(verifier.path().parent().unwrap()).unwrap();
    fs::write(verifier.path(), &before).unwrap();

    let (prompt, result) = answer_prompt(
        &verifier,
        &key_b(),
        "changed.test",
        22,
        HostKeyDecision::AcceptAndStore,
    )
    .await;
    assert!(prompt.changed);
    result.expect("shared CRLF record can be replaced safely");

    let result = fs::read_to_string(verifier.path()).unwrap();
    assert!(result.starts_with(&format!(
        "# retained comment\r\nother.test ssh-ed25519 {KEY_A}\r\n[changed.test]:2222 ssh-ed25519 {KEY_A}\r\n"
    )));
    assert_known(&verifier, &key_b(), "changed.test", 22).await;
    assert_known(&verifier, &key_a(), "other.test", 22).await;
    assert_known(&verifier, &key_a(), "changed.test", 2222).await;
    assert!(private_file_is_secure(verifier.path()).unwrap());
    assert_no_known_hosts_temporary_files(&verifier);
}

#[tokio::test]
async fn replacing_a_shared_record_without_final_newline_keeps_both_hosts_readable() {
    let temp = TempDir::new().unwrap();
    let verifier = verifier(&temp).with_changed_key_prompt();
    let before = format!("# retained comment\nchanged.test,other.test ssh-ed25519 {KEY_A}");
    fs::create_dir_all(verifier.path().parent().unwrap()).unwrap();
    fs::write(verifier.path(), before).unwrap();

    let (prompt, result) = answer_prompt(
        &verifier,
        &key_b(),
        "changed.test",
        22,
        HostKeyDecision::AcceptAndStore,
    )
    .await;
    assert!(prompt.changed);
    result.expect("last shared line needs a delimiter before learning the new key");

    let result = fs::read_to_string(verifier.path()).unwrap();
    assert!(result.starts_with(&format!(
        "# retained comment\nother.test ssh-ed25519 {KEY_A}\nchanged.test ssh-ed25519 {KEY_B}"
    )));
    assert_known(&verifier, &key_a(), "other.test", 22).await;
    assert_known(&verifier, &key_b(), "changed.test", 22).await;
    assert!(private_file_is_secure(verifier.path()).unwrap());
    assert_no_known_hosts_temporary_files(&verifier);
}

#[tokio::test]
async fn replacing_through_a_hashed_entry_fails_closed_and_keeps_the_file() {
    let temp = TempDir::new().unwrap();
    let verifier = verifier(&temp).with_changed_key_prompt();
    // A hashed record for a *different* host: it must never be touched by a replacement for
    // `changed.test`, but the shared line's hashed token makes token rewriting impossible.
    let hashed = format!(
        "|1|O33ESRMWPVkMYIwJ1Uw+n877jTo=|nuuC5vEqXlEZ/8BXQR7m619W6Ak=,changed.test \
         ssh-ed25519 {KEY_A}\n"
    );
    fs::create_dir_all(verifier.path().parent().unwrap()).unwrap();
    fs::write(verifier.path(), hashed).unwrap();
    let before = fs::read(verifier.path()).unwrap();

    let (prompt, result) = answer_prompt(
        &verifier,
        &key_b(),
        "changed.test",
        22,
        HostKeyDecision::AcceptAndStore,
    )
    .await;
    assert!(prompt.changed);
    assert!(
        matches!(result, Err(HostKeyError::Storage { .. })),
        "a hashed shared line must fail closed instead of being guessed"
    );
    assert_eq!(fs::read(verifier.path()).unwrap(), before);
    assert_no_known_hosts_temporary_files(&verifier);
}

#[tokio::test]
async fn matched_hashed_record_without_an_exact_endpoint_fails_closed() {
    let temp = TempDir::new().unwrap();
    let verifier = verifier(&temp).with_changed_key_prompt();
    // OpenSSH |1| HMAC-SHA1 hash of changed.test with a fixed salt.
    let hashed = format!(
        "|1|O33ESRMWPVkMYIwJ1Uw+n877jTo=|gdy8XDj83ad/InjejThl8SCiM2c= ssh-ed25519 {KEY_A}\n"
    );
    fs::create_dir_all(verifier.path().parent().unwrap()).unwrap();
    fs::write(verifier.path(), hashed).unwrap();
    let before = fs::read(verifier.path()).unwrap();

    let (prompt, result) = answer_prompt(
        &verifier,
        &key_b(),
        "changed.test",
        22,
        HostKeyDecision::AcceptAndStore,
    )
    .await;
    assert!(prompt.changed);
    assert!(matches!(result, Err(HostKeyError::Storage { .. })));
    assert_eq!(fs::read(verifier.path()).unwrap(), before);
    assert_no_known_hosts_temporary_files(&verifier);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_changed_key_acceptance_cannot_overwrite_a_newer_accepted_key() {
    let temp = TempDir::new().unwrap();
    let verifier = verifier(&temp)
        .with_changed_key_prompt()
        .with_timeout(Duration::from_secs(5));
    accept_unknown(&verifier, &key_a(), "changed.test", 22).await;
    let verifier_clone = verifier.clone();
    let verifier_second = verifier.clone();
    // Both distinct proposed keys see the same original pin before either response is sent.
    let stale_key = key_c();
    let new_key = key_b();
    let (broker_first, mut requests_first) = interaction_channel();
    let (broker_second, mut requests_second) = interaction_channel();
    let broker_first_response = broker_first.clone();
    let broker_second_response = broker_second.clone();
    let task_first = tokio::spawn(async move {
        verifier_clone
            .verify("changed.test", 22, &new_key, &broker_first_response)
            .await
    });
    let task_second = tokio::spawn(async move {
        verifier_second
            .verify("changed.test", 22, &stale_key, &broker_second_response)
            .await
    });
    let InteractionRequest::HostKey(prompt_first) =
        tokio::time::timeout(Duration::from_secs(2), requests_first.recv())
            .await
            .expect("first changed-key prompt timed out")
            .expect("first changed-key prompt channel closed")
            .1
    else {
        panic!("expected first changed-key prompt");
    };
    let InteractionRequest::HostKey(prompt_second) =
        tokio::time::timeout(Duration::from_secs(2), requests_second.recv())
            .await
            .expect("second changed-key prompt timed out")
            .expect("second changed-key prompt channel closed")
            .1
    else {
        panic!("expected second changed-key prompt");
    };
    assert!(prompt_first.changed && prompt_second.changed);

    // The first acceptance wins; the second one confirmed a state that no longer exists.
    broker_first
        .respond(
            prompt_first.id,
            InteractionResponse::HostKey(HostKeyDecision::AcceptAndStore),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), task_first)
        .await
        .expect("first acceptance timed out")
        .unwrap()
        .expect("first acceptance stores");
    let after_first = fs::read(verifier.path()).unwrap();
    broker_second
        .respond(
            prompt_second.id,
            InteractionResponse::HostKey(HostKeyDecision::AcceptAndStore),
        )
        .unwrap();
    assert!(
        matches!(
            tokio::time::timeout(Duration::from_secs(2), task_second)
                .await
                .expect("second acceptance timed out")
                .unwrap(),
            Err(HostKeyError::Changed { .. })
        ),
        "a stale acceptance must not overwrite the newer stored key"
    );
    assert_eq!(fs::read(verifier.path()).unwrap(), after_first);

    assert_known(&verifier, &key_b(), "changed.test", 22).await;
    let fail_closed = KnownHostsVerifier::new(verifier.path());
    let (broker, mut requests) = interaction_channel();
    assert!(matches!(
        fail_closed
            .verify("changed.test", 22, &key_a(), &broker)
            .await,
        Err(HostKeyError::Changed { .. })
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), requests.recv())
            .await
            .is_err(),
        "the overwritten key must not receive a new acceptance prompt"
    );
}

#[tokio::test]
async fn a_changed_key_acceptance_fails_if_the_recorded_key_disappeared() {
    let temp = TempDir::new().unwrap();
    let verifier = verifier(&temp).with_changed_key_prompt();
    accept_unknown(&verifier, &key_a(), "changed.test", 22).await;

    let (broker, mut requests) = interaction_channel();
    let proposed = key_b();
    let verification = verifier.verify("changed.test", 22, &proposed, &broker);
    tokio::pin!(verification);
    let request = tokio::select! {
        request = requests.recv() => request.expect("changed-key request"),
        result = &mut verification => panic!("unexpected verification result: {result:?}"),
    };
    let InteractionRequest::HostKey(prompt) = request.1 else {
        panic!("expected changed-key prompt");
    };
    assert!(prompt.changed);
    fs::write(verifier.path(), b"# now unknown\n").unwrap();
    let before = fs::read(verifier.path()).unwrap();
    broker
        .respond(
            prompt.id,
            InteractionResponse::HostKey(HostKeyDecision::AcceptAndStore),
        )
        .unwrap();

    assert!(matches!(
        verification.await,
        Err(HostKeyError::Changed { .. })
    ));
    assert_eq!(fs::read(verifier.path()).unwrap(), before);
    assert_no_known_hosts_temporary_files(&verifier);
}

#[tokio::test]
async fn a_changed_key_acceptance_rechecks_every_recorded_key_not_just_the_first_line() {
    let temp = TempDir::new().unwrap();
    let verifier = verifier(&temp).with_changed_key_prompt();
    fs::create_dir_all(verifier.path().parent().unwrap()).unwrap();
    fs::write(
        verifier.path(),
        format!("changed.test ssh-ed25519 {KEY_A}\nchanged.test ssh-ed25519 {KEY_B}\n"),
    )
    .unwrap();

    let proposed = key_c();
    let (broker, mut requests) = interaction_channel();
    let verification = verifier.verify("changed.test", 22, &proposed, &broker);
    tokio::pin!(verification);
    let request = tokio::select! {
        request = requests.recv() => request.expect("changed-key request"),
        result = &mut verification => panic!("unexpected verification result: {result:?}"),
    };
    let InteractionRequest::HostKey(prompt) = request.1 else {
        panic!("expected changed-key prompt");
    };
    assert!(prompt.changed);
    // The first matching line stays at the same number and retains its key.
    fs::write(
        verifier.path(),
        format!("changed.test ssh-ed25519 {KEY_A}\nchanged.test ssh-ed25519 {KEY_A}\n"),
    )
    .unwrap();
    let before = fs::read(verifier.path()).unwrap();
    broker
        .respond(
            prompt.id,
            InteractionResponse::HostKey(HostKeyDecision::AcceptAndStore),
        )
        .unwrap();

    assert!(matches!(
        verification.await,
        Err(HostKeyError::Changed { .. })
    ));
    assert_eq!(fs::read(verifier.path()).unwrap(), before);
    assert_no_known_hosts_temporary_files(&verifier);
}

#[tokio::test]
async fn accepting_the_same_changed_key_twice_is_idempotent() {
    let temp = TempDir::new().unwrap();
    let verifier = verifier(&temp).with_changed_key_prompt();
    accept_unknown(&verifier, &key_a(), "changed.test", 22).await;

    let (broker, mut requests) = interaction_channel();
    let proposed = key_b();
    let verification = verifier.verify("changed.test", 22, &proposed, &broker);
    tokio::pin!(verification);
    let request = tokio::select! {
        request = requests.recv() => request.expect("changed-key request"),
        result = &mut verification => panic!("unexpected verification result: {result:?}"),
    };
    let InteractionRequest::HostKey(prompt) = request.1 else {
        panic!("expected changed-key prompt");
    };
    assert!(prompt.changed);

    let (second_prompt, result) = answer_prompt(
        &verifier,
        &key_b(),
        "changed.test",
        22,
        HostKeyDecision::AcceptAndStore,
    )
    .await;
    assert!(second_prompt.changed);
    result.expect("first acceptance stores the new key");
    let before = fs::read(verifier.path()).unwrap();
    broker
        .respond(
            prompt.id,
            InteractionResponse::HostKey(HostKeyDecision::AcceptAndStore),
        )
        .unwrap();

    verification
        .await
        .expect("same proposed key is already known");
    assert_eq!(fs::read(verifier.path()).unwrap(), before);
    assert_no_known_hosts_temporary_files(&verifier);
}
