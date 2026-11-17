//! Where a session runs against a directory: below it, beside it, or in
//! another worktree of the same repository. The repositories are made up
//! in a temporary directory: `.git` directories and files, nothing more.

use std::fs;
use std::path::{Path, PathBuf};

use iktinos::place::{git_common_dir, is_within, Place};

/// An empty directory of this test's own, resolved (the temporary directory
/// may be behind a symbolic link).
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("iktinos-place-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(dir).unwrap()
}

/// A repository at `root`: a `.git` directory.
fn repository(root: &Path) {
    fs::create_dir_all(root.join(".git/worktrees")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
}

/// A linked worktree of the repository at `main`, at `at`: a `.git` file
/// that points to its git directory, which names the common one.
fn worktree(main: &Path, at: &Path, name: &str) {
    let git_dir = main.join(".git/worktrees").join(name);
    fs::create_dir_all(&git_dir).unwrap();
    fs::write(git_dir.join("commondir"), "../..\n").unwrap();
    fs::create_dir_all(at.join("src")).unwrap();
    fs::write(at.join(".git"), format!("gitdir: {}\n", git_dir.display())).unwrap();
}

#[test]
fn below_is_by_whole_components() {
    let foo = Path::new("/work/foo");
    assert!(is_within(foo, foo));
    assert!(is_within(Path::new("/work/foo/src/lib.rs"), foo));
    assert!(is_within(Path::new("/work/foo/"), foo));
    // A name that only starts the same is another directory.
    assert!(!is_within(Path::new("/work/foobar"), foo));
    assert!(!is_within(foo, Path::new("/work/foobar")));
    assert!(!is_within(Path::new("/work"), foo));
    assert!(!is_within(Path::new("work/foo"), foo));
}

#[test]
fn a_directory_holds_itself_and_what_is_below_it() {
    // None of these exist: they are compared as they are written.
    let mut place = Place::new(Path::new("/no-such-root/work/foo"));
    assert_eq!(place.dir(), Path::new("/no-such-root/work/foo"));
    assert!(place.holds(Path::new("/no-such-root/work/foo")));
    assert!(place.holds(Path::new("/no-such-root/work/foo/crates/core")));
    assert!(!place.holds(Path::new("/no-such-root/work/foobar")));
    assert!(!place.holds(Path::new("/no-such-root/work")));
    // Asked again, the same.
    assert!(place.holds(Path::new("/no-such-root/work/foo")));
    assert!(!place.holds(Path::new("/no-such-root/work/foobar")));
}

#[test]
fn the_common_git_directory_is_found_from_a_directory_and_from_a_file() {
    let root = scratch("common");
    let main = root.join("main");
    repository(&main);
    let linked = root.join("elsewhere/linked");
    worktree(&main, &linked, "linked");
    let common = main.join(".git");

    // A `.git` directory, from the root and from below it.
    assert_eq!(git_common_dir(&main), Some(common.clone()));
    assert_eq!(git_common_dir(&main.join("src")), Some(common.clone()));
    // A `.git` file: `gitdir:`, then `commondir`.
    assert_eq!(git_common_dir(&linked), Some(common.clone()));
    assert_eq!(git_common_dir(&linked.join("src")), Some(common.clone()));
    // A directory that is gone: from the nearest one that is there.
    assert_eq!(git_common_dir(&main.join("removed/deeper")), Some(common));

    // Outside any repository.
    assert_eq!(git_common_dir(&root.join("elsewhere")), None);

    // A `.git` file that leads nowhere, or says nothing, is no repository;
    // the search does not go on above it.
    let broken = main.join("broken");
    fs::create_dir_all(&broken).unwrap();
    fs::write(broken.join(".git"), "gitdir: /no-such-root/git\n").unwrap();
    assert_eq!(git_common_dir(&broken), None);
    fs::write(broken.join(".git"), "").unwrap();
    assert_eq!(git_common_dir(&broken), None);
    fs::write(broken.join(".git"), "not a pointer\n").unwrap();
    assert_eq!(git_common_dir(&broken), None);

    // A git directory without `commondir` (a submodule's) is its own.
    let module = root.join("module");
    let module_git = root.join("modules-git");
    fs::create_dir_all(&module).unwrap();
    fs::create_dir_all(&module_git).unwrap();
    fs::write(module.join(".git"), "gitdir: ../modules-git\n").unwrap();
    assert_eq!(git_common_dir(&module), Some(module_git));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn worktrees_of_one_repository_are_one_place_and_another_repository_is_not() {
    let root = scratch("worktrees");
    let main = root.join("main");
    repository(&main);
    let linked = root.join("linked");
    worktree(&main, &linked, "linked");
    let other = root.join("other");
    repository(&other);
    let plain = root.join("plain");
    fs::create_dir_all(&plain).unwrap();

    // From a subdirectory of the main worktree.
    let mut place = Place::new(&main.join("src"));
    assert!(place.holds(&main.join("src")));
    // The rest of the repository, though not below the directory.
    assert!(place.holds(&main));
    // Another worktree, whole.
    assert!(place.holds(&linked));
    assert!(place.holds(&linked.join("src")));
    assert!(!place.holds(&other));
    assert!(!place.holds(&plain));
    assert!(!place.holds(&root));
    // A worktree that was removed cannot be told from any other missing
    // directory.
    assert!(!place.holds(&root.join("removed-worktree")));

    // From the linked worktree, the same repository.
    let mut place = Place::new(&linked);
    assert!(place.holds(&main.join("src")));
    assert!(!place.holds(&other));

    // Outside a repository, only what is below.
    let mut place = Place::new(&plain);
    assert!(place.holds(&plain.join("deeper")));
    assert!(!place.holds(&main));

    let _ = fs::remove_dir_all(root);
}
