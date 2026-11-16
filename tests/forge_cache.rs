//! FORGE.md §7.1/§7.2: the on-disk forge cache and painting from it (F3).
#![cfg(unix)]
use dust::forge::cache::{Cache, CacheKey, Caps, Envelope};
use dust::forge::request::{Payload, Read};
use dust::forge::transport::HostKey;
use std::time::{Duration, Instant};

#[path = "support/forge_fake.rs"]
mod forge_fake;
#[path = "support/forge_screen.rs"]
mod forge_screen;
#[path = "support/records.rs"]
mod records;
use records::lists;
use forge_screen::*;

fn key(read: &Read) -> CacheKey {
    CacheKey::for_read("github.com", Some("example/demo"), read)
}

#[test]
fn entries_round_trip_and_a_corrupt_file_is_a_miss_never_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::at(dir.path());
    let labels = key(&Read::Labels);
    assert!(cache.store(
        &labels,
        Payload::Text("bug".into()),
        Duration::from_secs(60),
        Some("\"e1\"".into()),
        None
    ));
    let envelope: Envelope<Payload> = cache.get(&labels).unwrap();
    assert_eq!(envelope.data, Payload::Text("bug".into()));
    assert_eq!(envelope.etag.as_deref(), Some("\"e1\""));
    assert!(envelope.fresh());
    let file = labels.file(dir.path());
    assert!(file.starts_with(dir.path().join("github.com/example/demo/labels")));
    // No temporary is left behind by the atomic write.
    let siblings = std::fs::read_dir(file.parent().unwrap()).unwrap().count();
    assert_eq!(siblings, 1);

    for garbage in [&b"{ not json"[..], b"", b"\xff\xfe\x00", b"{\"schema\":1}"] {
        std::fs::write(&file, garbage).unwrap();
        assert!(cache.get::<Payload>(&labels).is_none());
        assert!(!file.exists(), "a corrupt entry is unlinked");
        cache.store(
            &labels,
            Payload::Text("bug".into()),
            Duration::from_secs(60),
            None,
            None,
        );
    }
    assert_eq!(cache.stats().corrupt, 4);
    // A newer schema (or another key hashed to the same name) is a miss too.
    let mut stale: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    stale["schema"] = 999.into();
    std::fs::write(&file, stale.to_string()).unwrap();
    assert!(cache.get::<Payload>(&labels).is_none());

    // A broken index is rebuilt by scanning the entries that are there.
    cache.store(
        &labels,
        Payload::Text("bug".into()),
        Duration::from_secs(60),
        None,
        None,
    );
    let directory = labels.directory(dir.path());
    std::fs::write(directory.join("index.json"), "garbage").unwrap();
    let index = cache.index(&directory);
    assert_eq!(index.entries.len(), 1);
    assert!(index.total_bytes > 0);
}

#[test]
fn touch_renews_and_invalidate_expires_without_touching_data() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::at(dir.path());
    let repo = key(&Read::Repository);
    cache.store(
        &repo,
        Payload::Text("demo".into()),
        Duration::from_secs(3600),
        Some("\"v1\"".into()),
        None,
    );
    let mut old: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(repo.file(dir.path())).unwrap()).unwrap();
    old["fetched_at"] = (old["fetched_at"].as_u64().unwrap() - 7_200_000).into();
    std::fs::write(repo.file(dir.path()), old.to_string()).unwrap();
    assert!(!cache.get::<Payload>(&repo).unwrap().fresh());
    assert!(cache.touch(&repo));
    let renewed = cache.get::<Payload>(&repo).unwrap();
    assert!(renewed.fresh());
    assert_eq!(renewed.data, Payload::Text("demo".into()));
    assert_eq!(renewed.etag.as_deref(), Some("\"v1\""));
    assert!(cache.invalidate(&repo));
    let expired = cache.get::<Payload>(&repo).unwrap();
    assert!(!expired.fresh());
    assert_eq!(expired.data, Payload::Text("demo".into()));
}

#[test]
fn caps_evict_least_recently_used_but_never_the_repository_or_large_logs() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::at(dir.path()).with_caps(Caps {
        repo: 40 * 1024,
        host: 1024 * 1024,
    });
    let big = "x".repeat(10 * 1024);
    let repo = key(&Read::Repository);
    cache.store(
        &repo,
        Payload::Text(big.clone()),
        Duration::from_secs(60),
        None,
        None,
    );
    for number in 1..=8 {
        cache.store(
            &key(&Read::Issue(number)),
            Payload::Text(big.clone()),
            Duration::from_secs(60),
            None,
            None,
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        cache.get::<Payload>(&repo).is_some(),
        "the repository is never evicted"
    );
    assert!(
        cache.get::<Payload>(&key(&Read::Issue(8))).is_some(),
        "the newest entry stays"
    );
    assert!(
        cache.get::<Payload>(&key(&Read::Issue(1))).is_none(),
        "the oldest entry goes"
    );
    let index = cache.index(&repo.directory(dir.path()));
    assert!(index.total_bytes <= 40 * 1024, "{}", index.total_bytes);
    assert!(cache.stats().evicted > 0);

    let log = key(&Read::JobLog(7u64.into()));
    let huge = "y".repeat(9 * 1024 * 1024);
    assert!(!cache.store(
        &log,
        Payload::Text(huge),
        Duration::from_secs(60),
        None,
        None
    ));
    assert!(cache.get::<Payload>(&log).is_none());
}

#[test]
fn host_wide_reads_share_one_directory_per_host() {
    let host = HostKey::new("GitHub.COM");
    let search = CacheKey::new(&host, None, &Read::Viewer);
    assert!(
        search
            .file(std::path::Path::new("/c"))
            .starts_with("/c/github.com/_host/viewer")
    );
}

#[test]
fn first_paint_comes_from_disk_while_the_forge_is_slow() {
    if fixture_child(
        "first_paint_comes_from_disk_while_the_forge_is_slow",
        serde_json::json!({"calls":[],"comments":["Cached fixture ready"],"issues":navigation_issues()}),
    ) {
        return;
    }
    {
        let mut screen = open();
        goto(&mut screen, 0, 1);
        wait(&mut screen, "Cached fixture ready");
        settle(&mut screen, Duration::from_millis(400));
    }
    assert!(
        entries(&cache_dir()).len() >= 3,
        "the first session wrote the repository, the list and the item"
    );
    // The last session was an hour ago, so everything shown is also checked again. The forge
    // holds that check of the list until released, so whatever the list shows meanwhile came
    // from disk: no timing decides it.
    age_cache(3600);
    update_state(|state| {
        state["read_gates"] = serde_json::json!([{"prefix": ["issue", "list"], "name": "slow-list"}]);
    });
    let started = Instant::now();
    let mut screen = open();
    let opened = started.elapsed();
    // The Git tab is on screen first, as in a real start; measure from the tab switch.
    let _ = view(&mut screen);
    let at = Instant::now();
    goto(&mut screen, 0, 1);
    wait_for(&mut screen, "cached rows", |frame| {
        lists(frame, 8, "Navigation issue 8") && lists(frame, 1, "Navigation issue 1")
    });
    let first_paint = at.elapsed();
    let frame = view(&mut screen);
    assert!(
        frame.contains("cached") || frame.contains("updated"),
        "{frame}"
    );
    assert!(
        !frame.contains("updated just now"),
        "the held list check cannot have finished:\n{frame}"
    );
    println!("measured: first paint from cache {first_paint:?} (screen open {opened:?}, list held)");
    // The background check finishes later and changes nothing visible.
    let before = view(&mut screen)
        .lines()
        .filter(|line| {
            // A record's title row: `│   ○ Navigation issue 3 …`, or `│▎  ○ …` under the cursor.
            line.strip_prefix('│')
                .map(|rest| rest.trim_start_matches([' ', '▸', '▎']))
                .is_some_and(|rest| rest.starts_with("○ Navigation issue"))
        })
        // The list's half: the reader beside it fills in from its own reads meanwhile.
        .map(|line| line.split("││").next().unwrap_or(line).to_owned())
        .collect::<Vec<_>>();
    release("slow-list");
    wait(&mut screen, "updated just now");
    let after = view(&mut screen)
        .lines()
        .filter(|line| {
            // A record's title row: `│   ○ Navigation issue 3 …`, or `│▎  ○ …` under the cursor.
            line.strip_prefix('│')
                .map(|rest| rest.trim_start_matches([' ', '▸', '▎']))
                .is_some_and(|rest| rest.starts_with("○ Navigation issue"))
        })
        // The list's half: the reader beside it fills in from its own reads meanwhile.
        .map(|line| line.split("││").next().unwrap_or(line).to_owned())
        .collect::<Vec<_>>();
    // Every cached record's title row: the seven navigation issues, the cursor's included.
    assert_eq!(before.len(), 7, "{before:#?}");
    assert_eq!(before, after);
}

#[test]
fn a_corrupt_cache_is_ignored_by_the_screen() {
    if fixture_child(
        "a_corrupt_cache_is_ignored_by_the_screen",
        serde_json::json!({"calls":[],"comments":["Corrupt fixture ready"],"issues":navigation_issues()}),
    ) {
        return;
    }
    {
        let mut screen = open();
        goto(&mut screen, 0, 1);
        wait(&mut screen, "Corrupt fixture ready");
        settle(&mut screen, Duration::from_millis(400));
    }
    let files = entries(&cache_dir());
    assert!(!files.is_empty());
    for (index, path) in files.iter().enumerate() {
        std::fs::write(
            path,
            if index % 2 == 0 {
                "{\"schema\":1,\"key\":"
            } else {
                "\u{0}"
            },
        )
        .unwrap();
    }
    let mut screen = open();
    goto(&mut screen, 0, 1);
    wait_for(&mut screen, "#8", |frame| lists(frame, 8, "Navigation issue 8"));
    wait(&mut screen, "Corrupt fixture ready");
}
