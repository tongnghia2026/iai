//! `export-public`: the public tree, what is committed at HEAD less the Pro
//! part.
//!
//! ```text
//! cargo xtask export-public <out-dir>
//! cargo xtask export-public --sync <clone of the public repository>
//! ```
//!
//! The Pro part is every Rust module declared behind `#[cfg(feature = "pro")]`
//! in the app (`src/`) or in a crate of the workspace (`crates/`), found by
//! reading the sources, so a module added there later needs no entry here,
//! plus the paths in [`PRIVATE`]. `Cargo.toml` loses `pro` from its default
//! features, so a plain build of the public tree is the core app. What is
//! under `.github/public/` is the public repository's own `.github/`. Each Pro
//! module's declaration gets `#[rustfmt::skip]` there: `cargo fmt` reads every
//! `mod x;` whatever its `cfg`, and stops at the first file it cannot find.
//!
//! The lock file is then brought in line with the manifests by cargo itself
//! (what only the Pro crate needed goes; nothing kept changes version).
//!
//! Only committed files are read (the blobs of HEAD): nothing untracked or
//! ignored can end up in the output. The output folder must be outside this
//! repository; without `--sync` it must be new or empty. With `--sync` it is a
//! clone of the public repository, whose files are replaced by the tree (its
//! `.git` is left alone): commit and push there afterwards. Nothing is pushed
//! or published here.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

/// Paths (a trailing slash means the whole folder) that stay private besides
/// the Pro modules themselves.
const PRIVATE: &[&str] = &[
    // Working notes: the plans, among them the Pro part's methods and key.
    "docs/planning/",
    "AGENTS.md",
    // The Pro part's models: what they are, how they were exported.
    "docs/AI_MODELS.md",
    "models/",
    "scripts/export_retouch_onnx.py",
    "scripts/export_sapiens2_seg_onnx.py",
    // The browser extension serves "AI online".
    "extension/",
    // The private repository's own workflows: releases are built there only.
    ".github/workflows/",
    "scripts/package_update.ps1",
    // The Pro part's core, commands and key: a crate of its own, which the
    // app's manifest and lock file below stop naming.
    "crates/iai-pro/",
];

/// What in the app's manifest names the Pro part's crate.
const PRO_CRATE_DEP: &str =
    "# The Pro part's core, commands and key (private repository only; the public
# one has no crates/iai-pro and no line here).
iai-pro = { path = \"crates/iai-pro\", optional = true }
";
const PRO_FEATURE_DEP: &str = "\"dep:iai-pro\", ";

/// Folders whose files are published under another path.
const OVERLAY: &[(&str, &str)] = &[(".github/public/", ".github/")];

const PRO_CFG: &str = "#[cfg(feature = \"pro\")]";
const FMT_SKIP: &str = "#[rustfmt::skip]";
const DEFAULT_WITH_PRO: &str = "default = [\"pro\"]\n";
const DEFAULT_WITHOUT: &str = "default = []\n";

pub fn run(args: Vec<String>) -> Result<(), String> {
    let (sync, out) = match args.as_slice() {
        [flag, out] if flag == "--sync" => (true, out),
        [out] if out != "--sync" => (false, out),
        _ => {
            return Err(
                "usage: cargo xtask export-public [--sync] <folder outside the repository>"
                    .to_owned(),
            );
        }
    };
    let root = super::workspace_root()?;
    let out = std::path::absolute(out).map_err(|e| format!("{out}: {e}"))?;
    if inside(&out, &root) {
        return Err("the output folder must be outside the repository".to_owned());
    }
    if sync {
        if !out.join(".git").is_dir() {
            return Err(format!(
                "--sync wants a clone of the public repository: {}",
                out.display()
            ));
        }
    } else if std::fs::read_dir(&out).is_ok_and(|mut d| d.next().is_some()) {
        return Err(format!(
            "the output folder must be new or empty: {}",
            out.display()
        ));
    }
    if !git(&root, &["status", "--porcelain"])?
        .trim_ascii()
        .is_empty()
    {
        return Err("commit or stash first: the export is of HEAD".to_owned());
    }

    let mut files = head_files(&root)?;
    let pro = pro_modules(&files)?;
    for path in PRIVATE.iter().chain(OVERLAY.iter().map(|(src, _)| src)) {
        if !files.keys().any(|f| is_under(f, &[path])) {
            return Err(format!("names nothing that is tracked: {path}"));
        }
    }
    let private: Vec<&str> = pro
        .iter()
        .map(String::as_str)
        .chain(PRIVATE.iter().copied())
        .collect();

    let cargo = files
        .get("Cargo.toml")
        .map(|f| String::from_utf8_lossy(&f.data).into_owned())
        .ok_or("no Cargo.toml at HEAD")?;
    for (what, line) in [
        ("the default features", DEFAULT_WITH_PRO),
        ("the iai-pro dependency", PRO_CRATE_DEP),
        ("the pro feature's iai-pro", PRO_FEATURE_DEP),
    ] {
        if cargo.matches(line).count() != 1 {
            return Err(format!(
                "Cargo.toml: {what} are not what this export expects"
            ));
        }
    }
    if let Some(f) = files.get_mut("Cargo.toml") {
        f.data = cargo
            .replace(DEFAULT_WITH_PRO, DEFAULT_WITHOUT)
            .replace(PRO_CRATE_DEP, "")
            .replace(PRO_FEATURE_DEP, "")
            .into_bytes();
    }

    let mut kept: BTreeMap<String, File> = BTreeMap::new();
    for (path, file) in &files {
        if let Some((src, dst)) = OVERLAY.iter().find(|(src, _)| path.starts_with(src)) {
            kept.insert(format!("{dst}{}", &path[src.len()..]), file.clone());
        } else if !is_under(path, &private) {
            kept.entry(path.clone()).or_insert_with(|| file.clone());
        }
    }
    // A Pro module the sources still declare must be one that was left out.
    for prefix in &pro {
        if kept.keys().any(|f| is_under(f, &[prefix])) {
            return Err(format!("a Pro module would be published: {prefix}"));
        }
    }
    let mut skipped = 0;
    for (path, file) in kept.iter_mut() {
        if is_rust_source(path) {
            let text = String::from_utf8_lossy(&file.data);
            let (marked, n) = skip_pro_mods(&text);
            if n > 0 {
                skipped += n;
                file.data = marked.into_bytes();
            }
        }
    }

    if sync {
        for entry in std::fs::read_dir(&out).map_err(|e| format!("{}: {e}", out.display()))? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_name() == ".git" {
                continue;
            }
            let path = entry.path();
            let gone = if path.is_dir() && !path.is_symlink() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            gone.map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    for (path, file) in &kept {
        let dest = path.split('/').fold(out.clone(), |p, part| p.join(part));
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&dest, &file.data).map_err(|e| format!("{}: {e}", dest.display()))?;
        #[cfg(unix)]
        if file.executable {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("{}: {e}", dest.display()))?;
        }
    }
    settle_lock(&out)?;

    let head = git(&root, &["rev-parse", "--short", "HEAD"])?;
    let left: Vec<&String> = files
        .keys()
        .filter(|f| !OVERLAY.iter().any(|(src, _)| is_under(f, &[src])) && !kept.contains_key(*f))
        .collect();
    println!(
        "public tree of {} written to {}",
        String::from_utf8_lossy(&head).trim(),
        out.display()
    );
    println!("  {skipped} Pro module declarations marked {FMT_SKIP}");
    println!("  {} files kept, {} left out:", kept.len(), left.len());
    for prefix in &private {
        let n = left.iter().filter(|f| is_under(f, &[prefix])).count();
        println!("    {n:4}  {prefix}");
    }
    Ok(())
}

#[derive(Clone)]
struct File {
    data: Vec<u8>,
    #[cfg_attr(not(unix), allow(dead_code))]
    executable: bool,
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .args(["-c", "core.autocrlf=false"])
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

/// Every regular file committed at HEAD, by path, as stored.
fn head_files(root: &Path) -> Result<BTreeMap<String, File>, String> {
    let listing = git(root, &["ls-tree", "-r", "-z", "HEAD"])?;
    // `<mode> blob <sha>\t<path>`; links and submodules are left out, as an
    // archive's regular files would leave them.
    let mut entries = Vec::new();
    for entry in listing.split(|&b| b == 0).filter(|e| !e.is_empty()) {
        let entry = String::from_utf8_lossy(entry);
        let (meta, path) = entry
            .split_once('\t')
            .ok_or_else(|| format!("git ls-tree: {entry}"))?;
        let mut meta = meta.split(' ');
        let (Some(mode), Some(kind), Some(sha)) = (meta.next(), meta.next(), meta.next()) else {
            return Err(format!("git ls-tree: {entry}"));
        };
        if kind == "blob" && (mode == "100644" || mode == "100755") {
            entries.push((path.to_owned(), sha.to_owned(), mode == "100755"));
        }
    }

    let mut cat = Command::new("git")
        .args(["cat-file", "--batch"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git cat-file: {e}"))?;
    let mut stdin = cat.stdin.take().ok_or("git cat-file: no stdin")?;
    let shas: Vec<String> = entries.iter().map(|(_, sha, _)| sha.clone()).collect();
    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        for sha in shas {
            writeln!(stdin, "{sha}")?;
        }
        Ok(())
    });
    let mut reader = BufReader::new(cat.stdout.take().ok_or("git cat-file: no stdout")?);
    let mut files = BTreeMap::new();
    for (path, sha, executable) in entries {
        let mut header = String::new();
        reader
            .read_line(&mut header)
            .map_err(|e| format!("git cat-file: {e}"))?;
        // `<sha> blob <size>`
        let size: usize = header
            .trim_end()
            .rsplit(' ')
            .next()
            .and_then(|s| s.parse().ok())
            .filter(|_| header.starts_with(&sha))
            .ok_or_else(|| format!("git cat-file: {path}: {}", header.trim_end()))?;
        let mut data = vec![0; size];
        reader
            .read_exact(&mut data)
            .map_err(|e| format!("git cat-file: {path}: {e}"))?;
        let mut newline = [0u8; 1];
        reader
            .read_exact(&mut newline)
            .map_err(|e| format!("git cat-file: {e}"))?;
        files.insert(path, File { data, executable });
    }
    writer
        .join()
        .map_err(|_| "git cat-file: the writer stopped".to_owned())?
        .map_err(|e| format!("git cat-file: {e}"))?;
    cat.wait().map_err(|e| format!("git cat-file: {e}"))?;
    Ok(files)
}

fn is_rust_source(path: &str) -> bool {
    (path.starts_with("src/") || path.starts_with("crates/")) && path.ends_with(".rs")
}

/// `path` is one of `prefixes`, or inside one ending in `/`.
fn is_under(path: &str, prefixes: &[&str]) -> bool {
    prefixes
        .iter()
        .any(|p| path == *p || (p.ends_with('/') && path.starts_with(p)))
}

/// `out` is `root` or inside it (letter case aside, as on Windows).
fn inside(out: &Path, root: &Path) -> bool {
    let parts = |p: &Path| -> Vec<String> {
        p.components()
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .collect()
    };
    let (out, root) = (parts(out), parts(root));
    out.len() >= root.len() && out.iter().zip(&root).all(|(a, b)| a == b)
}

/// Where the child modules of the source file `path` live.
fn module_dir(path: &str) -> String {
    let (head, name) = path.rsplit_once('/').unwrap_or(("", path));
    if matches!(name, "mod.rs" | "lib.rs" | "main.rs") {
        head.to_owned()
    } else {
        path.trim_end_matches(".rs").to_owned()
    }
}

/// A Pro module declaration found in a source: the line of its `mod x;`,
/// its indent, the line ending of the `cfg` line, and the name.
struct ProDecl {
    mod_line: usize,
    indent: String,
    newline: &'static str,
    name: String,
}

/// The Pro module declarations of `text`: a line ending in
/// `#[cfg(feature = "pro")]`, any further attribute lines, then
/// `[pub[(…)]] mod name;`.
fn pro_decls(text: &str) -> Vec<ProDecl> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let body = |line: &str| {
        line.trim_end_matches('\n')
            .trim_end_matches('\r')
            .to_owned()
    };
    let mut found = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if !line.ends_with('\n') || !body(line).trim_end_matches([' ', '\t']).ends_with(PRO_CFG) {
            i += 1;
            continue;
        }
        let newline = if line.ends_with("\r\n") { "\r\n" } else { "\n" };
        let mut j = i + 1;
        while j < lines.len() && lines[j].ends_with('\n') && is_attribute_line(&body(lines[j])) {
            j += 1;
        }
        match lines.get(j).and_then(|l| mod_decl(&body(l))) {
            Some((indent, name)) => {
                found.push(ProDecl {
                    mod_line: j,
                    indent,
                    newline,
                    name,
                });
                i = j + 1;
            }
            None => i += 1,
        }
    }
    found
}

fn is_attribute_line(line: &str) -> bool {
    let line = line
        .trim_start_matches([' ', '\t'])
        .trim_end_matches([' ', '\t']);
    line.starts_with("#[") && line.ends_with(']')
}

/// `[indent][pub[(…)] ]mod name;…`: the indent and the name.
fn mod_decl(line: &str) -> Option<(String, String)> {
    let rest = line.trim_start_matches([' ', '\t']);
    let indent = line[..line.len() - rest.len()].to_owned();
    let mut rest = rest;
    if let Some(after) = rest.strip_prefix("pub") {
        let after = match after.strip_prefix('(') {
            Some(inner) => &inner[inner.find(')')? + 1..],
            None => after,
        };
        let trimmed = after.trim_start_matches([' ', '\t']);
        if trimmed.len() == after.len() {
            return None;
        }
        rest = trimmed;
    }
    let after = rest.strip_prefix("mod")?;
    let name_start = after.trim_start_matches([' ', '\t']);
    if name_start.len() == after.len() {
        return None;
    }
    let name: String = name_start
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        return None;
    }
    name_start[name.len()..]
        .trim_start_matches([' ', '\t'])
        .starts_with(';')
        .then_some((indent, name))
}

/// Path prefixes of the modules declared behind the `pro` feature.
fn pro_modules(files: &BTreeMap<String, File>) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for (path, file) in files {
        if !is_rust_source(path) {
            continue;
        }
        for decl in pro_decls(&String::from_utf8_lossy(&file.data)) {
            let base = format!("{}/{}", module_dir(path), decl.name);
            let mut found = Vec::new();
            if files.contains_key(&format!("{base}.rs")) {
                found.push(format!("{base}.rs"));
            }
            let dir = format!("{base}/");
            if files.keys().any(|t| t.starts_with(&dir)) {
                found.push(dir);
            }
            if found.is_empty() {
                return Err(format!(
                    "{path}: no file for the Pro module `{}`",
                    decl.name
                ));
            }
            out.extend(found);
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// `text` with `#[rustfmt::skip]` just above each Pro module's `mod x;`, and
/// how many were marked.
fn skip_pro_mods(text: &str) -> (String, usize) {
    let decls = pro_decls(text);
    if decls.is_empty() {
        return (text.to_owned(), 0);
    }
    let mut out = String::with_capacity(text.len() + decls.len() * 24);
    let mut next = decls.iter().peekable();
    for (n, line) in text.split_inclusive('\n').enumerate() {
        if let Some(decl) = next.next_if(|d| d.mod_line == n) {
            out.push_str(&decl.indent);
            out.push_str(FMT_SKIP);
            out.push_str(decl.newline);
        }
        out.push_str(line);
    }
    (out, decls.len())
}

/// Bring the lock file of the tree at `out` in line with its manifests, as
/// cargo does on any build: the Pro crate's entries go, and with them what
/// only it needed (cargo knows which features asked for what). Nothing kept
/// changes version. Offline first, from what this machine has downloaded.
fn settle_lock(out: &Path) -> Result<(), String> {
    for offline in [true, false] {
        let mut cargo = super::cargo();
        cargo
            .current_dir(out)
            .args(["metadata", "--format-version", "1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if offline {
            cargo.arg("--offline");
        }
        if cargo.status().map_err(|e| format!("cargo: {e}"))?.success() {
            return Ok(());
        }
    }
    Err(format!(
        "cargo metadata in {} failed: the public tree does not resolve",
        out.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pro_module_is_found_behind_its_cfg_and_other_attributes() {
        let src = "mod a;\n#[cfg(feature = \"pro\")]\n#[allow(dead_code)]\npub(crate) mod b;\n\
                   #[cfg(feature = \"pro\")]\r\n    pub mod c; // x\r\n#[cfg(feature = \"pro\")]\n\
                   fn d() {}\n#[cfg(feature = \"pro\")] mod e;\n";
        let names: Vec<String> = pro_decls(src).into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["b", "c"]);
        let (marked, n) = skip_pro_mods(src);
        assert_eq!(n, 2);
        assert!(marked.contains("#[allow(dead_code)]\n#[rustfmt::skip]\npub(crate) mod b;\n"));
        assert!(marked.contains("\r\n    #[rustfmt::skip]\r\n    pub mod c; // x\r\n"));
    }

    #[test]
    fn a_module_s_children_live_beside_it_or_in_its_folder() {
        assert_eq!(module_dir("src/pro/mod.rs"), "src/pro");
        assert_eq!(module_dir("src/lib.rs"), "src");
        assert_eq!(module_dir("src/app/state.rs"), "src/app/state");
        assert!(is_under("docs/planning/x.md", &["docs/planning/"]));
        assert!(!is_under("docs/planning.md", &["docs/planning/"]));
    }

    #[test]
    fn the_output_may_not_be_inside_the_repository() {
        let root = Path::new("/Users/A/IAI");
        assert!(inside(Path::new("/users/a/iai/out"), root));
        assert!(!inside(Path::new("/Users/A/IAI-public"), root));
    }
}
