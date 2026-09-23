//! Our plugins write only into their own data directory.
//!
//! A convention and not a lock (every plugin runs under the same account), so
//! it is held here instead: no plugin crate's non-test source may spell a path
//! under /etc/ritornello or /var/lib/ritornello, except the few that are not
//! data. The pattern is assembled at runtime so this file does not match
//! itself — the same care `langpack::archive`'s own source guard takes.

#[test]
fn no_plugin_spells_a_data_path_of_its_own() {
    let etc = ["/etc/", "ritornello/"].concat();
    let var = ["/var/lib/", "ritornello/"].concat();
    // Installed files (not data), and the root helper's fixed, documented
    // location of the files plugin's directory.
    let allowed = [
        ["/etc/ritornello/", "input-presets"].concat(),
        ["/var/lib/ritornello/", "plugins/files"].concat(),
    ];
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut offenders = Vec::new();
    let mut scanned = 0;
    for entry in std::fs::read_dir(crates).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("ritornello-plugin-") || name == "ritornello-plugin-sdk" {
            continue;
        }
        for file in walk(&entry.path().join("src")) {
            let text = std::fs::read_to_string(&file).unwrap();
            // Only the production half: everything from the first `#[cfg(test)]`
            // on is test code, where literal paths are fixtures.
            let prod = text.split("#[cfg(test)]").next().unwrap();
            scanned += 1;
            for (i, line) in prod.lines().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                let hit = (line.contains(&etc) || line.contains(&var))
                    && !allowed.iter().any(|a| line.contains(a.as_str()));
                if hit {
                    offenders.push(format!("{}:{}: {}", file.display(), i + 1, line.trim()));
                }
            }
        }
    }
    assert!(scanned > 10, "scanned only {scanned} files — the walk is wrong");
    assert!(
        offenders.is_empty(),
        "a plugin spells a data path instead of using ritornello_plugin_sdk::data_dir():\n{}",
        offenders.join("\n")
    );
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    out
}
