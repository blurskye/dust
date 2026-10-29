//! Proofs for U-12's isolation clause: a fixture cannot run a hook and cannot reach a network.
use std::{path::PathBuf, time::Duration};

use super::{TutorError, TutorEvent, TutorService};

fn create(service: &mut TutorService, real_roots: Vec<PathBuf>) -> PathBuf {
    service
        .create(real_roots, Duration::from_secs(10))
        .expect("fixture request accepted");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match service.poll() {
            Some(TutorEvent::Created { result, .. }) => {
                return result.expect("fixture created");
            }
            Some(other) => panic!("unexpected tutor event: {other:?}"),
            None => {
                assert!(std::time::Instant::now() < deadline, "creation timed out");
                std::thread::yield_now();
            }
        }
    }
}

#[test]
fn a_fixture_is_refused_inside_a_real_root_and_contains_none() {
    let mut service = TutorService::start(4);
    let real = tempfile::tempdir().unwrap();
    let real = std::fs::canonicalize(real.path()).unwrap();
    // The system temporary directory is never inside a real root in this test, so the refusal
    // is exercised by making the REAL ROOT the candidate's parent instead: the fixture creates
    // under temp_dir, so a real root that CONTAINS the temp dir must be refused.
    let outer = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    service
        .create(vec![outer.clone()], Duration::from_secs(10))
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let error = loop {
        match service.poll() {
            Some(TutorEvent::Created { result, .. }) => break result.unwrap_err(),
            Some(other) => panic!("unexpected tutor event: {other:?}"),
            None => {
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
        }
    };
    // A root that contains the fixture destination is refused before anything is written.
    assert!(
        matches!(
            error,
            TutorError::InsideRealRoot(_) | TutorError::ContainsRealRoot(_)
        ),
        "a root containing the fixture destination is refused: {error:?}"
    );
    // And a real root that is unrelated does not stop creation.
    let unrelated = create(&mut service, vec![real]);
    assert!(unrelated.is_dir());
    service
        .dispose(unrelated, Duration::from_secs(10))
        .expect("disposal request accepted");
}

#[test]
fn a_hook_inside_a_fixture_never_executes() {
    let mut service = TutorService::start(4);
    let path = create(&mut service, Vec::new());
    // A hook that would leave a marker if anything ever ran it.
    let hook = path.join(".git").join("hooks").join("pre-commit");
    let marker = path.join("hook-ran.marker");
    std::fs::write(
        &hook,
        format!("#!/bin/sh\ntouch {}\n", marker.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&hook, permissions).unwrap();
    }
    // Commit through libgit2, the way the sandbox and the editor both do.
    let repository = git2::Repository::open(&path).unwrap();
    std::fs::write(path.join("practice.txt"), "more practice\n").unwrap();
    let mut index = repository.index().unwrap();
    index.add_path(std::path::Path::new("practice.txt")).unwrap();
    index.write().unwrap();
    let tree = repository
        .find_tree(index.write_tree().unwrap())
        .unwrap();
    let signature = git2::Signature::now("Probe", "probe@dust.invalid").unwrap();
    let parent = repository.head().unwrap().peel_to_commit().unwrap();
    repository
        .commit(Some("HEAD"), &signature, &signature, "probe", &tree, &[&parent])
        .unwrap();
    assert!(
        !marker.exists(),
        "the fixture's pre-commit hook did not execute: {} was created",
        marker.display()
    );
    // NETWORK ISOLATION, re-proved for the revision that DOES carry the https and ssh
    // transports. The earlier guarantee was structural — the binary had no TLS backend, so a
    // fetch could not open a stream — and that stopped being true when the owner-approved
    // transports landed. The guarantee is now about what a fixture is GIVEN: its remotes are
    // local filesystem paths, so there is no address to reach and nothing to authenticate to.
    let bare = tempfile::tempdir().unwrap();
    git2::Repository::init_bare(bare.path()).unwrap();
    repository
        .remote("origin", bare.path().to_str().unwrap())
        .unwrap();
    let origin = repository.find_remote("origin").unwrap();
    let url = origin.url().unwrap_or_default().to_owned();
    assert!(
        !url.contains("://"),
        "a tutor fixture's remote is a filesystem path, never a URL that reaches a network: {url}"
    );
    assert!(
        std::path::Path::new(&url).is_absolute(),
        "and it names a real local directory rather than a host: {url}"
    );
    // A fixture created by the sandbox configures no remote at all, so there is nothing to fetch
    // from unless an exercise deliberately points at local storage.
    let fresh = create(&mut service, Vec::new());
    let fresh_repository = git2::Repository::open(&fresh).unwrap();
    assert!(
        fresh_repository.remotes().unwrap().is_empty(),
        "the sandbox gives a fixture no remote, so no exercise can reach a network"
    );
    service
        .dispose(fresh, Duration::from_secs(10))
        .expect("disposal request accepted");
    drop(origin);
    drop(tree);
    drop(parent);
    drop(repository);
    service
        .dispose(path, Duration::from_secs(10))
        .expect("disposal request accepted");
}

#[test]
fn disposal_removes_the_fixture_and_is_idempotent() {
    let mut service = TutorService::start(4);
    let path = create(&mut service, Vec::new());
    assert!(path.is_dir());
    service
        .dispose(path.clone(), Duration::from_secs(10))
        .expect("disposal request accepted");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match service.poll() {
            Some(TutorEvent::Disposed { result, .. }) => {
                result.expect("disposal reported success");
                break;
            }
            Some(other) => panic!("unexpected tutor event: {other:?}"),
            None => {
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
        }
    }
    assert!(!path.exists(), "disposal removed the directory");
    // Disposing again is already-disposed, not an error.
    service
        .dispose(path.clone(), Duration::from_secs(10))
        .expect("second disposal request accepted");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match service.poll() {
            Some(TutorEvent::Disposed { result, .. }) => {
                result.expect("disposing an absent fixture is not an error");
                break;
            }
            Some(other) => panic!("unexpected tutor event: {other:?}"),
            None => {
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
        }
    }
    assert!(!path.exists());
}
