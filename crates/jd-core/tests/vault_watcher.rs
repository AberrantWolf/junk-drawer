//! Watcher contract (spec §2 "external edits are legal", §13 editor zoo).
//! Platform FS latency varies wildly: tests poll with generous deadlines and
//! assert semantics, not exact event counts.

mod common;

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::TempDir;
use jd_core::vault::Vault;
use jd_core::vault::watcher::{VaultWatcher, WatchEvent};

/// Collect events until `pred` is satisfied or 3 s pass. Returns all seen.
fn wait_for(
    rx: &mpsc::Receiver<WatchEvent>,
    pred: impl Fn(&[WatchEvent]) -> bool,
) -> Vec<WatchEvent> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(50)) {
            seen.push(ev);
        }
        if pred(&seen) {
            break;
        }
    }
    seen
}

fn changed_paths(evs: &[WatchEvent]) -> Vec<&Path> {
    evs.iter()
        .filter_map(|e| match e {
            WatchEvent::Changed(p) => Some(p.as_path()),
            _ => None,
        })
        .collect()
}

fn setup() -> (TempDir, Vault, VaultWatcher, mpsc::Receiver<WatchEvent>) {
    let t = TempDir::new();
    let v = Vault::open(t.path()).unwrap();
    let (tx, rx) = mpsc::channel();
    let w = VaultWatcher::start(&v, tx).unwrap();
    // let the OS watcher settle before mutating
    std::thread::sleep(Duration::from_millis(250));
    (t, v, w, rx)
}

#[test]
fn direct_write_is_changed() {
    let (t, _v, _w, rx) = setup();
    std::fs::write(t.path().join("notes/new.md"), "# New\n").unwrap();
    let evs = wait_for(&rx, |s| !changed_paths(s).is_empty());
    assert!(
        changed_paths(&evs).contains(&Path::new("notes/new.md")),
        "{evs:?}"
    );
}

#[test]
fn rename_swap_save_normalizes_to_changed() {
    // vim-style: write a sidecar, rename over the target
    let (t, _v, _w, rx) = setup();
    let target = t.path().join("notes/note.md");
    std::fs::write(&target, "v1\n").unwrap();
    let _ = wait_for(&rx, |s| !changed_paths(s).is_empty());

    let sidecar = t.path().join("notes/note.md.swp-like");
    std::fs::write(&sidecar, "v2\n").unwrap();
    std::fs::rename(&sidecar, &target).unwrap();
    let evs = wait_for(&rx, |s| {
        changed_paths(s).contains(&Path::new("notes/note.md"))
    });
    assert!(
        changed_paths(&evs).contains(&Path::new("notes/note.md")),
        "{evs:?}"
    );
    // the sidecar itself must not surface (non-.md name)
    assert!(!evs.iter().any(|e| matches!(e, WatchEvent::Changed(p) | WatchEvent::Removed(p) if p.to_string_lossy().contains("swp"))));
}

#[test]
fn truncate_rewrite_is_changed_and_delete_is_removed() {
    let (t, _v, _w, rx) = setup();
    let target = t.path().join("inbox/scrap.md");
    std::fs::write(&target, "first\n").unwrap();
    let _ = wait_for(&rx, |s| !changed_paths(s).is_empty());

    std::fs::write(&target, "rewritten\n").unwrap(); // truncate+rewrite
    let evs = wait_for(&rx, |s| {
        changed_paths(s).contains(&Path::new("inbox/scrap.md"))
    });
    assert!(changed_paths(&evs).contains(&Path::new("inbox/scrap.md")));

    std::fs::remove_file(&target).unwrap();
    let evs = wait_for(&rx, |s| {
        s.iter()
            .any(|e| matches!(e, WatchEvent::Removed(p) if p == Path::new("inbox/scrap.md")))
    });
    assert!(
        evs.iter()
            .any(|e| matches!(e, WatchEvent::Removed(p) if p == Path::new("inbox/scrap.md"))),
        "{evs:?}"
    );
}

#[test]
fn create_then_rename_lands_on_the_final_name() {
    let (t, _v, _w, rx) = setup();
    let tmp_name = t.path().join("notes/draft.md");
    std::fs::write(&tmp_name, "content\n").unwrap();
    std::fs::rename(&tmp_name, t.path().join("notes/Final.md")).unwrap();
    let evs = wait_for(&rx, |s| {
        changed_paths(s).contains(&Path::new("notes/Final.md"))
    });
    assert!(
        changed_paths(&evs).contains(&Path::new("notes/Final.md")),
        "{evs:?}"
    );
}

#[test]
fn junkdrawer_and_tempfiles_are_invisible() {
    let (t, _v, _w, rx) = setup();
    std::fs::write(t.path().join(".junkdrawer/session/state.jd"), "x").unwrap();
    std::fs::write(t.path().join("notes/.real.md.jd-tmp"), "x").unwrap();
    std::fs::write(t.path().join("notes/visible.md"), "x").unwrap();
    let evs = wait_for(&rx, |s| !changed_paths(s).is_empty());
    for e in &evs {
        let p = match e {
            WatchEvent::Changed(p) | WatchEvent::Removed(p) => p,
            WatchEvent::Renamed { to, .. } => to,
        };
        assert_eq!(p, Path::new("notes/visible.md"), "leaked event: {e:?}");
    }
}

#[test]
fn burst_coalesces_after_quiet() {
    let (t, _v, _w, rx) = setup();
    let target = t.path().join("notes/busy.md");
    for i in 0..10 {
        std::fs::write(&target, format!("rev {i}\n")).unwrap();
        std::thread::sleep(Duration::from_millis(10));
    }
    // wait out the debounce, then drain
    std::thread::sleep(Duration::from_millis(600));
    let evs = wait_for(&rx, |s| !s.is_empty());
    let for_busy = changed_paths(&evs)
        .iter()
        .filter(|p| **p == Path::new("notes/busy.md"))
        .count();
    assert!(for_busy >= 1, "burst produced nothing: {evs:?}");
    assert!(
        for_busy <= 3,
        "debounce isn't coalescing (got {for_busy}): {evs:?}"
    );
}
