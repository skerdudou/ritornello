use super::*;
use ritornello_files_mount::roots::{Root, RootKind, Roots};
use std::time::Instant;

const D: Duration = Duration::from_secs(5);

fn root(name: &str, kind: RootKind, writable: bool) -> Root {
    Root {
        name: name.into(),
        kind,
        path: (kind == RootKind::Local).then(|| "/srv/music".to_string()),
        host: "nas".into(),
        share: "music".into(),
        subpath: None,
        user: String::new(),
        domain: String::new(),
        writable,
        archive_covers: false,
        archive_lyrics: false,
    }
}

fn roots(list: Vec<Root>) -> Roots {
    Roots { root: list }
}

/// What the kernel says: `rw` and `ro` shares, a read-write one that is not
/// declared writable, and a local root that happens to have a line of its own.
const MOUNTS: &str = "\
//nas/music /mnt/ritornello/rw cifs rw,relatime,vers=3.1.1 0 0
//nas/music /mnt/ritornello/ro cifs ro,relatime,vers=3.1.1 0 0
//nas/music /mnt/ritornello/locked cifs rw,relatime 0 0
/dev/sda1 /mnt/ritornello/local ext4 rw 0 0
";

fn table() -> Roots {
    roots(vec![
        root("rw", RootKind::Smb, true),
        root("ro", RootKind::Smb, true),
        root("locked", RootKind::Smb, false),
        root("unmounted", RootKind::Smb, true),
        root("local", RootKind::Local, true),
    ])
}

fn same(p: &Path) -> io::Result<PathBuf> {
    Ok(p.to_path_buf())
}

fn target(root: &str, path: &str) -> Target {
    Target { root: root.into(), path: path.into() }
}

// ---- files_roots_path ------------------------------------------------------

#[test]
fn the_roots_file_is_the_files_plugins_next_to_our_own_data_dir() {
    assert_eq!(
        files_roots_path(Path::new("/data/plugins/web-streaming")),
        PathBuf::from("/data/plugins").join("files").join("media-roots.toml")
    );
}

// ---- mounted_rw: one test per branch ---------------------------------------

#[test]
fn rw_is_writable() {
    assert!(mounted_rw(MOUNTS, Path::new("/mnt/ritornello/rw")));
}

#[test]
fn ro_is_not_writable() {
    assert!(!mounted_rw(MOUNTS, Path::new("/mnt/ritornello/ro")));
}

#[test]
fn rw_inside_another_option_is_not_rw() {
    for opts in ["ro,user=rw", "ro,rwx", "ro,xrw", "ro,rw-ish", "ro,uid=rw,gid=rw"] {
        let text = format!("//nas/x /mnt/ritornello/x cifs {opts} 0 0\n");
        assert!(!mounted_rw(&text, Path::new("/mnt/ritornello/x")), "{opts}");
    }
}

#[test]
fn rw_is_found_wherever_it_sits_among_the_options() {
    for opts in ["rw", "rw,relatime", "relatime,rw", "relatime,rw,vers=3"] {
        let text = format!("//nas/x /mnt/ritornello/x cifs {opts} 0 0\n");
        assert!(mounted_rw(&text, Path::new("/mnt/ritornello/x")), "{opts}");
    }
}

#[test]
fn a_mount_point_with_an_escaped_space_is_found_and_judged() {
    let text = "//nas/x /mnt/ritornello/ma\\040musique cifs rw 0 0\n";
    assert!(mounted_rw(text, Path::new("/mnt/ritornello/ma musique")));
    let ro = "//nas/x /mnt/ritornello/ma\\040musique cifs ro 0 0\n";
    assert!(!mounted_rw(ro, Path::new("/mnt/ritornello/ma musique")));
}

#[test]
fn a_trailing_space_in_the_mount_point_is_not_the_same_point() {
    let text = "//nas/x /mnt/ritornello/x\\040 cifs rw 0 0\n";
    assert!(mounted_rw(text, Path::new("/mnt/ritornello/x ")));
    assert!(!mounted_rw(text, Path::new("/mnt/ritornello/x")));
}

#[test]
fn rw_is_read_from_the_options_column_and_not_from_the_device() {
    // `rw` as the device name or the mount point's neighbour columns.
    let text = "rw /mnt/ritornello/x cifs ro 0 0\n//nas/x /mnt/ritornello/y rw ro 0 0\n";
    assert!(!mounted_rw(text, Path::new("/mnt/ritornello/x")));
    assert!(!mounted_rw(text, Path::new("/mnt/ritornello/y")));
}

#[test]
fn an_unmounted_point_is_not_rw_and_a_short_line_is_not_a_panic() {
    assert!(!mounted_rw(MOUNTS, Path::new("/mnt/ritornello/unmounted")));
    assert!(!mounted_rw("//nas/x /mnt/ritornello/x cifs\n", Path::new("/mnt/ritornello/x")));
    assert!(!mounted_rw("", Path::new("/mnt/ritornello/x")));
}

#[test]
fn the_last_line_of_a_point_is_the_effective_mount() {
    let remounted_ro = "//nas/x /mnt/ritornello/x cifs rw 0 0\n//nas/x /mnt/ritornello/x cifs ro 0 0\n";
    assert!(!mounted_rw(remounted_ro, Path::new("/mnt/ritornello/x")));
    let remounted_rw = "//nas/x /mnt/ritornello/x cifs ro 0 0\n//nas/x /mnt/ritornello/x cifs rw 0 0\n";
    assert!(mounted_rw(remounted_rw, Path::new("/mnt/ritornello/x")));
}

// ---- writable_targets: one test per exclusion ------------------------------

#[test]
fn a_writable_root_mounted_rw_is_a_target() {
    assert_eq!(writable_targets(&table(), MOUNTS), vec!["rw".to_string()]);
}

#[test]
fn a_root_not_declared_writable_is_excluded() {
    assert!(!writable_targets(&table(), MOUNTS).contains(&"locked".to_string()));
}

#[test]
fn a_writable_root_mounted_ro_is_excluded() {
    assert!(!writable_targets(&table(), MOUNTS).contains(&"ro".to_string()));
}

#[test]
fn a_root_that_is_not_mounted_is_excluded() {
    assert!(!writable_targets(&table(), MOUNTS).contains(&"unmounted".to_string()));
}

#[test]
fn a_local_root_is_excluded_even_when_writable_and_rw() {
    assert!(!writable_targets(&table(), MOUNTS).contains(&"local".to_string()));
}

// ---- resolve_with: the root-level checks -----------------------------------

#[tokio::test]
async fn a_good_target_resolves_under_the_base_dir_not_the_mount_point() {
    let mut r = root("rw", RootKind::Smb, true);
    r.subpath = Some("Ma Musique".into());
    let roots = roots(vec![r]);
    let got = resolve_with(&target("rw", ""), &roots, MOUNTS, D, same).await.unwrap();
    assert_eq!(got, PathBuf::from("/mnt/ritornello/rw/Ma Musique"));
    let got = resolve_with(&target("rw", "Albums/Rock "), &roots, MOUNTS, D, same).await.unwrap();
    assert_eq!(got, PathBuf::from("/mnt/ritornello/rw/Ma Musique/Albums/Rock "));
}

#[tokio::test]
async fn an_unknown_root_is_refused() {
    let r = resolve_with(&target("nope", "a"), &table(), MOUNTS, D, same).await;
    assert_eq!(r, Err(TargetError::UnknownRoot));
}

#[tokio::test]
async fn a_root_not_declared_writable_is_refused() {
    let r = resolve_with(&target("locked", "a"), &table(), MOUNTS, D, same).await;
    assert_eq!(r, Err(TargetError::NotWritable));
}

#[tokio::test]
async fn a_writable_root_mounted_ro_is_refused() {
    let r = resolve_with(&target("ro", "a"), &table(), MOUNTS, D, same).await;
    assert_eq!(r, Err(TargetError::NotWritable));
}

#[tokio::test]
async fn a_root_that_is_not_mounted_is_refused() {
    let r = resolve_with(&target("unmounted", "a"), &table(), MOUNTS, D, same).await;
    assert_eq!(r, Err(TargetError::NotMounted));
}

#[tokio::test]
async fn a_local_root_is_refused() {
    let r = resolve_with(&target("local", "a"), &table(), MOUNTS, D, same).await;
    assert_eq!(r, Err(TargetError::NotWritable));
}

// ---- the path forms --------------------------------------------------------

async fn path_form(path: &str) -> Result<PathBuf, TargetError> {
    resolve_with(&target("rw", path), &table(), MOUNTS, D, same).await
}

#[tokio::test]
async fn dot_dot_is_refused() {
    assert_eq!(path_form("..").await, Err(TargetError::Escapes));
    assert_eq!(path_form("../x").await, Err(TargetError::Escapes));
    assert_eq!(path_form("a/../b").await, Err(TargetError::Escapes));
}

#[tokio::test]
async fn a_single_dot_is_refused() {
    assert_eq!(path_form(".").await, Err(TargetError::Escapes));
    assert_eq!(path_form("a/./b").await, Err(TargetError::Escapes));
}

#[tokio::test]
async fn an_empty_component_is_refused() {
    assert_eq!(path_form("/a").await, Err(TargetError::Escapes));
    assert_eq!(path_form("a//b").await, Err(TargetError::Escapes));
    assert_eq!(path_form("a/").await, Err(TargetError::Escapes));
}

#[tokio::test]
async fn the_empty_path_is_the_base_dir_itself() {
    assert_eq!(path_form("").await, Ok(PathBuf::from("/mnt/ritornello/rw")));
}

#[tokio::test]
async fn a_nul_byte_is_refused() {
    assert_eq!(path_form("a\0b").await, Err(TargetError::Escapes));
    assert_eq!(path_form("\0").await, Err(TargetError::Escapes));
}

#[tokio::test]
async fn a_backslash_is_refused() {
    assert_eq!(path_form("a\\b").await, Err(TargetError::Escapes));
}

#[tokio::test]
async fn nested_folders_given_with_slashes_are_accepted() {
    assert_eq!(path_form("a/b").await, Ok(PathBuf::from("/mnt/ritornello/rw/a/b")));
}

// ---- confinement on a real filesystem --------------------------------------

#[tokio::test]
async fn a_symlink_that_points_outside_the_base_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, base.join("escape")).unwrap();
    let r = confine(base, "escape", D, |p| std::fs::canonicalize(p)).await;
    assert_eq!(r.unwrap_err(), TargetError::Escapes);
}

#[tokio::test]
async fn a_symlink_that_points_inside_the_base_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    std::fs::create_dir_all(base.join("real")).unwrap();
    std::os::unix::fs::symlink(base.join("real"), base.join("alias")).unwrap();
    let got = confine(base.clone(), "alias", D, |p| std::fs::canonicalize(p)).await.unwrap();
    assert_eq!(got.dir, std::fs::canonicalize(base.join("real")).unwrap());
}

#[tokio::test]
async fn a_symlink_in_the_middle_of_the_path_is_followed_and_judged() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::create_dir_all(outside.join("deep")).unwrap();
    std::os::unix::fs::symlink(&outside, base.join("hop")).unwrap();
    let r = confine(base, "hop/deep", D, |p| std::fs::canonicalize(p)).await;
    assert_eq!(r.unwrap_err(), TargetError::Escapes);
}

#[tokio::test]
async fn a_base_that_is_itself_a_symlink_is_compared_by_its_real_path() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir_all(real.join("sub")).unwrap();
    let base = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &base).unwrap();
    let got = confine(base, "sub", D, |p| std::fs::canonicalize(p)).await.unwrap();
    assert_eq!(got.dir, std::fs::canonicalize(real.join("sub")).unwrap());
}

#[tokio::test]
async fn a_sibling_sharing_the_base_name_as_a_prefix_is_outside() {
    // `starts_with` on components, not on the string: `base-old` is not in `base`.
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::create_dir_all(dir.path().join("base-old")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("base-old"), base.join("sib")).unwrap();
    let r = confine(base, "sib", D, |p| std::fs::canonicalize(p)).await;
    assert_eq!(r.unwrap_err(), TargetError::Escapes);
}

#[tokio::test]
async fn a_missing_folder_is_an_io_error_not_an_escape() {
    let dir = tempfile::tempdir().unwrap();
    let r = confine(dir.path().to_path_buf(), "nope", D, |p| std::fs::canonicalize(p)).await;
    assert!(matches!(r, Err(TargetError::Io(_))), "{r:?}");
}

#[tokio::test]
async fn listing_a_dead_share_times_out() {
    let dead = |p: &Path| {
        std::thread::sleep(Duration::from_secs(2));
        Ok(p.to_path_buf())
    };
    let started = Instant::now();
    let r = resolve_with(&target("rw", "a"), &table(), MOUNTS, Duration::from_millis(100), dead).await;
    assert_eq!(r, Err(TargetError::Unresponsive));
    assert!(started.elapsed() < Duration::from_millis(500), "{:?}", started.elapsed());
}

// ---- list_dirs / create_dir -------------------------------------------------

#[tokio::test]
async fn listing_gives_the_visible_real_folders_sorted() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().to_path_buf();
    for d in ["beta", "Alpha", ".hidden", "gamma"] {
        std::fs::create_dir(base.join(d)).unwrap();
    }
    std::fs::write(base.join("file.mp3"), b"x").unwrap();
    std::os::unix::fs::symlink(base.join("beta"), base.join("alias")).unwrap();
    let got = list_dirs_in(base, "", D, |p| std::fs::canonicalize(p)).await.unwrap();
    assert_eq!(got, vec!["Alpha".to_string(), "beta".into(), "gamma".into()]);
}

#[tokio::test]
async fn listing_a_dead_share_is_unresponsive() {
    let dir = tempfile::tempdir().unwrap();
    let dead = |p: &Path| {
        std::thread::sleep(Duration::from_secs(2));
        Ok(p.to_path_buf())
    };
    let started = Instant::now();
    let r = list_dirs_in(dir.path().to_path_buf(), "", Duration::from_millis(100), dead).await;
    assert_eq!(r, Err(TargetError::Unresponsive));
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[tokio::test]
async fn creating_a_folder_makes_exactly_one_level_inside_the_target() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().to_path_buf();
    std::fs::create_dir(base.join("a")).unwrap();
    create_dir_in(base.clone(), "a", "new one", D, |p| std::fs::canonicalize(p)).await.unwrap();
    assert!(base.join("a/new one").is_dir());
}

#[tokio::test]
async fn a_folder_name_is_a_single_plain_component() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    std::fs::create_dir_all(&base).unwrap();
    for bad in ["a/b", "..", ".", "", "a\0b", "a\\b", "../x", "/abs"] {
        let r = create_dir_in(base.clone(), "", bad, D, |p| std::fs::canonicalize(p)).await;
        assert_eq!(r, Err(TargetError::Escapes), "{bad:?}");
    }
    assert_eq!(std::fs::read_dir(&base).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "nothing was made beside the base");
}

#[tokio::test]
async fn creating_over_a_symlink_never_follows_it() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, base.join("hop")).unwrap();
    let r = create_dir_in(base, "", "hop", D, |p| std::fs::canonicalize(p)).await;
    assert!(matches!(r, Err(TargetError::Io(_))), "{r:?}");
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
}

#[tokio::test]
async fn create_dir_and_list_dirs_refuse_what_resolve_refuses() {
    let n = Duration::from_secs(1);
    assert_eq!(create_dir(&target("ro", ""), "x", &table(), MOUNTS, n).await, Err(TargetError::NotWritable));
    assert_eq!(list_dirs(&target("nope", ""), &table(), MOUNTS, n).await, Err(TargetError::UnknownRoot));
    assert_eq!(create_dir(&target("rw", ".."), "x", &table(), MOUNTS, n).await, Err(TargetError::Escapes));
}

// ---- `$`: yt-dlp expands it in the -o value -----------------------------------

#[tokio::test]
async fn a_component_with_a_dollar_is_refused() {
    assert_eq!(path_form("$HOME").await, Err(TargetError::Escapes));
    assert_eq!(path_form("a/${PATH}").await, Err(TargetError::Escapes));
    assert_eq!(path_form("cash$").await, Err(TargetError::Escapes));
    let r = create_dir(&target("rw", ""), "$HOME", &table(), MOUNTS, D).await;
    assert_eq!(r, Err(TargetError::Escapes));
}

#[tokio::test]
async fn a_base_dir_with_a_dollar_is_refused() {
    let mut r = root("rw", RootKind::Smb, true);
    r.subpath = Some("Music$".into());
    let roots = roots(vec![r]);
    let got = resolve_with(&target("rw", ""), &roots, MOUNTS, D, same).await;
    assert_eq!(got, Err(TargetError::Escapes));
    // Even when the real path no longer shows it (the base is a symlink): the
    // declared base is judged too.
    let clean = |p: &Path| Ok(PathBuf::from(p.to_string_lossy().replace('$', "S")));
    let got = resolve_with(&target("rw", "x"), &roots, MOUNTS, D, clean).await;
    assert_eq!(got, Err(TargetError::Escapes));
}

#[tokio::test]
async fn a_symlink_inside_the_base_to_a_folder_whose_real_name_has_a_dollar_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    std::fs::create_dir_all(base.join("real$dir")).unwrap();
    std::os::unix::fs::symlink(base.join("real$dir"), base.join("alias")).unwrap();
    let r = confine(base, "alias", D, |p| std::fs::canonicalize(p)).await;
    assert_eq!(r.unwrap_err(), TargetError::Escapes);
}

#[tokio::test]
async fn a_base_whose_real_path_has_a_dollar_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("b$");
    std::fs::create_dir_all(base.join("sub")).unwrap();
    let r = confine(base, "sub", D, |p| std::fs::canonicalize(p)).await;
    assert_eq!(r.unwrap_err(), TargetError::Escapes);
}

#[tokio::test]
async fn a_folder_with_a_dollar_is_not_offered() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("cash$")).unwrap();
    std::fs::create_dir(dir.path().join("fine")).unwrap();
    let got = list_dirs_in(dir.path().to_path_buf(), "", D, |p| std::fs::canonicalize(p)).await.unwrap();
    assert_eq!(got, vec!["fine".to_string()]);
}

#[tokio::test]
async fn a_new_folder_may_not_start_with_a_dot() {
    let dir = tempfile::tempdir().unwrap();
    let r = create_dir_in(dir.path().to_path_buf(), "", ".hidden", D, |p| std::fs::canonicalize(p)).await;
    assert_eq!(r, Err(TargetError::Escapes));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    // A dot elsewhere in the name is fine.
    create_dir_in(dir.path().to_path_buf(), "", "v1.0", D, |p| std::fs::canonicalize(p)).await.unwrap();
}
