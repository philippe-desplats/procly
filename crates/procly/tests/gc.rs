mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use common::{Procly, assert_success, free_port, git, git_repository, stderr, stdout, test_server};
use procly_client::protocol::{GcReport, Orphan, OrphanId, Request, Response};
use procly_client::state_dir::StateDir;

/// A `procly.toml` declaring `web`, which runs the test server on its leased `http` port, then `extra`.
fn web(extra: &str) -> String {
    format!(
        "[services.web]\nrun = '{} --port {{ports.http}}'\nports = {{ http = {} }}\n{extra}",
        test_server().display(),
        free_port()
    )
}

/// A setup command that always fails, so that `worktree init` keeps the setup directory of the Worktree.
const FAILING_SETUP: &str = "[worktree]\nsetup = ['echo failing; exit 3']\n";

/// Gives `worktree` a Lease, an output directory and, with [`FAILING_SETUP`], a setup directory, then stops its
/// Service. Returns the port of its Lease.
fn leave_behind(procly: &Procly, worktree: &Path, config: &str) -> u16 {
    fs::write(worktree.join("procly.toml"), config).expect("write the config file");
    if config.contains("[worktree]") {
        let init = procly.procly_in(worktree, &["worktree", "init"]);
        assert_eq!(init.status.code(), Some(3), "{}", stderr(&init));
    }
    let up = procly.procly_in(worktree, &["up", "web"]);
    assert_success(&up);
    let port = stdout(&up)
        .trim_end()
        .rsplit(' ')
        .next()
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| panic!("no port in {:?}", stdout(&up)));
    assert_success(&procly.procly_in(worktree, &["down", "web"]));
    port
}

/// The directories of `kind`, `output` or `setup`, the state directory holds for a Worktree.
fn worktree_dirs(procly: &Procly, kind: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(procly.state_dir().join(kind)) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .map(|entry| entry.expect("read the directory").path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// The one directory of `kind` whose name starts with the slug `slug`.
fn worktree_dir(procly: &Procly, kind: &str, slug: &str) -> PathBuf {
    let found: Vec<PathBuf> = worktree_dirs(procly, kind)
        .into_iter()
        .filter(|dir| {
            dir.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&format!("{slug}-")))
        })
        .collect();
    assert_eq!(found.len(), 1, "{kind} directories of {slug}: {found:?}");
    found.into_iter().next().expect("one directory")
}

fn leases(procly: &Procly) -> String {
    fs::read_to_string(procly.state_dir().join("leases.json")).expect("the Leases are stored")
}

/// What the Daemon answers to a `Gc` request the CLI cannot send, `only` being the one field it offers no flag for.
fn gc_report(procly: &Procly, apply: bool, only: Option<Vec<OrphanId>>) -> GcReport {
    let state = StateDir::at(procly.state_dir());
    let request = Request::Gc {
        cwd: procly.scratch().to_owned(),
        apply,
        only,
    };
    let answer = procly_client::connect(&state)
        .expect("the Daemon runs")
        .send(&request)
        .expect("send the request")
        .answer(|_| {})
        .expect("the Daemon answers");
    match answer {
        Response::Gc(report) => report,
        other => panic!("not a Gc answer: {other:?}"),
    }
}

/// The identities of `orphans` that belong to `worktree`, as a client keeps them from a dry run.
fn ids_of(orphans: &[Orphan], worktree: &Path) -> Vec<OrphanId> {
    orphans
        .iter()
        .filter(|orphan| orphan.id.worktree == worktree)
        .map(|orphan| orphan.id.clone())
        .collect()
}

fn remove_worktree(main: &Path, worktree: &Path) {
    git(main, &["worktree", "remove", "--force", &worktree.to_string_lossy()]);
}

#[test]
fn a_removed_worktree_leaves_orphans_that_gc_lists_and_gc_apply_deletes() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let (gone, kept) = (
        common::git_worktree_add(&main, "app-gone"),
        common::git_worktree_add(&main, "app-kept"),
    );
    let config = web(FAILING_SETUP);
    let port = leave_behind(&procly, &gone, &config);
    let kept_port = leave_behind(&procly, &kept, &config);
    let (output_dir, setup_dir) = (
        worktree_dir(&procly, "output", "app_gone"),
        worktree_dir(&procly, "setup", "app_gone"),
    );
    remove_worktree(&main, &gone);

    let listed = procly.procly(&["gc"]);

    assert_success(&listed);
    let gone_text = gone.display();
    assert_eq!(
        stdout(&listed),
        format!(
            "lease web.http port {port} of {gone_text}\noutput directory {} of {gone_text}\nsetup directory {} of \
             {gone_text}\n",
            output_dir.display(),
            setup_dir.display()
        )
    );
    assert_eq!(stderr(&listed), "procly: 3 Orphans, `procly gc --apply` deletes them\n");
    assert!(
        output_dir.exists() && setup_dir.exists(),
        "gc without --apply deleted something"
    );
    assert!(leases(&procly).contains(&format!("\"port\": {port}")));

    let applied = procly.procly(&["gc", "--apply"]);

    assert_success(&applied);
    assert_eq!(
        stdout(&applied),
        format!(
            "deleted lease web.http port {port} of {gone_text}\ndeleted output directory {} of {gone_text}\ndeleted \
             setup directory {} of {gone_text}\n",
            output_dir.display(),
            setup_dir.display()
        )
    );
    assert!(!output_dir.exists() && !setup_dir.exists());
    let leases = leases(&procly);
    assert!(!leases.contains(&format!("\"port\": {port}")), "{leases}");
    // A Worktree git still lists keeps all it holds.
    assert!(leases.contains(&format!("\"port\": {kept_port}")), "{leases}");
    worktree_dir(&procly, "output", "app_kept");
    worktree_dir(&procly, "setup", "app_kept");
    let log = procly.daemon_log();
    assert!(
        log.contains(&format!(
            "gc deleted lease web.http port {port} of {gone_text}, requested by pid "
        )),
        "{log}"
    );
    assert!(
        log.contains(&format!(
            "gc deleted setup directory {} of {gone_text}, requested by pid ",
            setup_dir.display()
        )),
        "{log}"
    );

    let again = procly.procly(&["gc"]);
    assert_success(&again);
    assert_eq!(stdout(&again), "no Orphans\n");
}

#[test]
fn a_prunable_worktree_is_no_orphan_until_git_prunes_it() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let worktree = common::git_worktree_add(&main, "app-unmounted");
    let port = leave_behind(&procly, &worktree, &web(""));
    // Gone from the disk without git knowing, as a Worktree on a disk that is not mounted.
    fs::remove_dir_all(&worktree).expect("remove the Worktree directory");
    assert!(common::git_output(&main, &["worktree", "list", "--porcelain"]).contains("prunable"));

    let listed = procly.procly(&["gc", "--apply"]);

    assert_success(&listed);
    assert_eq!(stdout(&listed), "no Orphans\n");
    assert!(leases(&procly).contains(&format!("\"port\": {port}")));

    git(&main, &["worktree", "prune"]);
    let pruned = procly.procly(&["gc"]);

    assert_success(&pruned);
    assert!(
        stdout(&pruned).starts_with(&format!("lease web.http port {port} of {}\n", worktree.display())),
        "{}",
        stdout(&pruned)
    );
}

#[test]
fn nothing_a_pinned_branch_holds_is_listed_or_deleted() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let path = main
        .parent()
        .expect("the Main worktree has a parent")
        .join("app-develop");
    git(
        &main,
        &["worktree", "add", "--quiet", "-b", "develop", &path.to_string_lossy()],
    );
    let develop = path.canonicalize().expect("canonicalize the worktree");
    let port = leave_behind(&procly, &develop, &web("[pinned.develop]\n"));
    let output_dir = worktree_dir(&procly, "output", "app_develop");
    remove_worktree(&main, &develop);

    let applied = procly.procly(&["gc", "--apply"]);

    assert_success(&applied);
    // The output directory belongs to the removed Worktree, the Lease to the branch.
    assert_eq!(
        stdout(&applied),
        format!(
            "deleted output directory {} of {}\n",
            output_dir.display(),
            develop.display()
        )
    );
    let leases = leases(&procly);
    assert!(
        leases.contains("\"branch\": \"develop\"") && leases.contains(&format!("\"port\": {port}")),
        "{leases}"
    );
}

#[test]
fn a_failed_deletion_is_reported_and_the_others_still_happen_exiting_6() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let gone = common::git_worktree_add(&main, "app-gone");
    let port = leave_behind(&procly, &gone, &web(FAILING_SETUP));
    let (output_dir, setup_dir) = (
        worktree_dir(&procly, "output", "app_gone"),
        worktree_dir(&procly, "setup", "app_gone"),
    );
    remove_worktree(&main, &gone);
    // The output directory cannot be taken out of a directory the Daemon may not write.
    let outputs = procly.state_dir().join("output");
    fs::set_permissions(&outputs, fs::Permissions::from_mode(0o500)).expect("make the output directory read-only");

    let applied = procly.procly(&["gc", "--apply"]);

    fs::set_permissions(&outputs, fs::Permissions::from_mode(0o700)).expect("make the output directory writable");
    assert_eq!(applied.status.code(), Some(6), "{}", stderr(&applied));
    let gone_text = gone.display();
    assert_eq!(
        stdout(&applied),
        format!(
            "deleted lease web.http port {port} of {gone_text}\ndeleted setup directory {} of {gone_text}\n",
            setup_dir.display()
        )
    );
    let stderr = stderr(&applied);
    assert!(
        stderr.starts_with(&format!(
            "procly: cannot delete output directory {} of {gone_text}: cannot remove {}: ",
            output_dir.display(),
            output_dir.display()
        )),
        "{stderr}"
    );
    assert!(
        stderr.ends_with("procly: 1 of 3 Orphans could not be deleted\n"),
        "{stderr}"
    );
    assert!(output_dir.exists() && !setup_dir.exists());
    assert!(
        procly
            .daemon_log()
            .contains(&format!("gc cannot delete output directory {}", output_dir.display())),
        "{}",
        procly.daemon_log()
    );

    let retried = procly.procly(&["gc", "--apply"]);
    assert_success(&retried);
    assert!(!output_dir.exists());
}

#[test]
fn gc_and_gc_apply_answer_with_one_json_document() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let gone = common::git_worktree_add(&main, "app-gone");
    let port = leave_behind(&procly, &gone, &web(""));
    remove_worktree(&main, &gone);
    let schema_version = common::json_schema_version(&procly);

    let listed = common::json_succeeded(&procly.procly(&["gc", "--json"]));

    assert_eq!(listed["schema_version"], schema_version);
    assert_eq!(listed["applied"], false);
    assert_eq!(listed["skipped"], serde_json::json!([]));
    let orphans = listed["orphans"].as_array().expect("orphans is a list");
    assert_eq!(orphans.len(), 2, "{listed}");
    assert_eq!(
        orphans[0],
        serde_json::json!({
            "kind": "lease",
            "service": "web",
            "port_name": "http",
            "port": port,
            "repository": main.join(".git"),
            "worktree": gone,
            "deleted": false,
            "error": null,
        })
    );
    assert_eq!(orphans[1]["kind"], "output_directory");

    let applied = common::json_succeeded(&procly.procly(&["gc", "--apply", "--json"]));

    assert_eq!(applied["applied"], true);
    assert!(
        applied["orphans"]
            .as_array()
            .expect("orphans is a list")
            .iter()
            .all(|orphan| orphan["deleted"] == true && orphan["error"].is_null()),
        "{applied}"
    );
    assert_eq!(
        common::json_succeeded(&procly.procly(&["gc", "--json"]))["orphans"],
        serde_json::json!([])
    );
}

#[test]
fn gc_apply_limited_to_the_listed_orphans_deletes_only_those() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let (first, second) = (
        common::git_worktree_add(&main, "app-first"),
        common::git_worktree_add(&main, "app-second"),
    );
    let (first_port, second_port) = (
        leave_behind(&procly, &first, &web("")),
        leave_behind(&procly, &second, &web("")),
    );
    remove_worktree(&main, &first);
    remove_worktree(&main, &second);
    let shown = gc_report(&procly, false, None);
    assert_eq!(shown.orphans.len(), 4, "{shown:?}");
    let only = ids_of(&shown.orphans, &first);
    assert_eq!(only.len(), 2, "{only:?}");

    let applied = gc_report(&procly, true, Some(only));

    assert!(applied.applied);
    assert_eq!(applied.orphans.len(), 2, "{applied:?}");
    assert!(
        applied
            .orphans
            .iter()
            .all(|orphan| orphan.id.worktree == first && orphan.deleted && orphan.error.is_none()),
        "{applied:?}"
    );
    let leases = leases(&procly);
    assert!(!leases.contains(&format!("\"port\": {first_port}")), "{leases}");
    assert!(leases.contains(&format!("\"port\": {second_port}")), "{leases}");
    assert_eq!(worktree_dirs(&procly, "output").len(), 1);
    let left = gc_report(&procly, false, None);
    assert_eq!(left.orphans.len(), 2, "{left:?}");
    assert!(left.orphans.iter().all(|orphan| orphan.id.worktree == second));
}

#[test]
fn an_orphan_of_only_that_no_longer_is_one_is_reported_as_not_deleted_with_its_reason() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let (gone, later) = (
        common::git_worktree_add(&main, "app-gone"),
        common::git_worktree_add(&main, "app-later"),
    );
    leave_behind(&procly, &gone, &web(""));
    remove_worktree(&main, &gone);
    let shown = gc_report(&procly, false, None).orphans;
    // Collected by someone else after the sheet showed it, and a Worktree that is removed since.
    assert_success(&procly.procly(&["gc", "--apply"]));
    let later_port = leave_behind(&procly, &later, &web(""));
    remove_worktree(&main, &later);
    let mut only: Vec<OrphanId> = shown.iter().map(|orphan| orphan.id.clone()).collect();
    let still = gc_report(&procly, false, None).orphans;
    only.extend(still.iter().take(1).map(|orphan| orphan.id.clone()));

    let applied = gc_report(&procly, true, Some(only));

    let (gone_ones, deleted): (Vec<&Orphan>, Vec<&Orphan>) =
        applied.orphans.iter().partition(|orphan| orphan.id.worktree == gone);
    assert_eq!(gone_ones.len(), shown.len(), "{applied:?}");
    assert!(
        gone_ones
            .iter()
            .all(|orphan| !orphan.deleted && orphan.error.as_deref() == Some("no longer an Orphan")),
        "{applied:?}"
    );
    assert_eq!(deleted.len(), 1, "{applied:?}");
    assert!(deleted[0].deleted && deleted[0].error.is_none(), "{applied:?}");
    assert!(applied.failed());
    // What `only` did not list stays.
    let left = gc_report(&procly, false, None).orphans;
    assert_eq!(left.len(), still.len() - 1, "{left:?}");
    // The Lease comes first among the Orphans, and is the one `only` listed.
    assert!(!leases(&procly).contains(&format!("\"port\": {later_port}")));
}

#[test]
fn only_lists_nothing_to_delete_when_none_of_its_orphans_remains() {
    let procly = Procly::new();
    procly.narrow_range();
    let main = git_repository(procly.scratch(), "app");
    let gone = common::git_worktree_add(&main, "app-gone");
    leave_behind(&procly, &gone, &web(""));
    remove_worktree(&main, &gone);

    let none = gc_report(&procly, true, Some(Vec::new()));

    assert!(none.orphans.is_empty(), "{none:?}");
    assert_eq!(gc_report(&procly, false, None).orphans.len(), 2);
}
