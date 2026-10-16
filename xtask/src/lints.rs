//! Clippy over the workspace with a budget for the crash lints.
//!
//! The workspace denies nothing it cannot keep: `unwrap`, `expect`, `panic!`
//! and unsafe blocks without a `SAFETY:` note are warnings in Cargo.toml,
//! because the app still has some. This check fails on every other warning
//! (as `-D warnings` would) and on a crash lint that a crate has more of than
//! `xtask/lint-budget.txt` allows. A crate not in the file is allowed none.
//! After a cleanup, `--update` lowers the numbers; it never raises them.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::Stdio;

use serde_json::Value;

const BUDGET_FILE: &str = "xtask/lint-budget.txt";

/// The lints a crate may have a budget for.
const BUDGETED: &[&str] = &[
    "clippy::unwrap_used",
    "clippy::expect_used",
    "clippy::panic",
    "clippy::todo",
    "clippy::unimplemented",
    "clippy::unreachable",
    "clippy::undocumented_unsafe_blocks",
];

/// (crate, lint) → count.
type Counts = BTreeMap<(String, String), u64>;

pub fn run(args: Vec<String>) -> Result<(), String> {
    let update = args.iter().any(|a| a == "--update");
    let cargo_args: Vec<&String> = args.iter().filter(|a| *a != "--update").collect();
    // A build with less in it has fewer of everything: lowered from there,
    // the budget would fail the build as shipped.
    if update && !as_shipped(&cargo_args) {
        return Err(format!(
            "--update counts the build as shipped: cargo xtask lints --update {SHIPPED}"
        ));
    }

    let path = budget_path()?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{BUDGET_FILE}: {e}"))?;
    let budget = parse_budget(&text)?;

    let mut child = crate::cargo()
        .args([
            "clippy",
            "--workspace",
            "--locked",
            "--message-format=json-diagnostic-rendered-ansi",
        ])
        .args(&cargo_args)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cargo clippy: {e}"))?;
    let stdout = child.stdout.take().ok_or("cargo clippy: no output")?;

    let mut counts = Counts::new();
    let mut other = Vec::new();
    let mut errors = 0u64;
    let mut warnings = 0u64;
    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|e| format!("cargo clippy: {e}"))?;
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if msg["reason"].as_str() != Some("compiler-message") {
            continue;
        }
        let diag = &msg["message"];
        let level = diag["level"].as_str().unwrap_or_default();
        let code = diag["code"]["code"].as_str();
        match (level, code) {
            ("error", _) => {
                errors += 1;
                other.push(rendered(diag));
            }
            ("warning", Some(code)) if BUDGETED.contains(&code) => {
                let krate = package_name(msg["package_id"].as_str().unwrap_or_default());
                *counts.entry((krate, code.to_owned())).or_default() += 1;
            }
            // rustc's closing "N warnings emitted" line.
            ("warning", None) if diag["spans"].as_array().is_some_and(|s| s.is_empty()) => {}
            ("warning", _) => {
                warnings += 1;
                other.push(rendered(diag));
            }
            _ => {}
        }
    }
    let status = child.wait().map_err(|e| format!("cargo clippy: {e}"))?;

    let mut over = Vec::new();
    let keys: std::collections::BTreeSet<_> = counts.keys().chain(budget.keys()).collect();
    println!(
        "{:<14} {:<36} {:>6} {:>6}",
        "crate", "lint", "now", "budget"
    );
    for key in keys {
        let now = counts.get(key).copied().unwrap_or(0);
        let allowed = budget.get(key).copied().unwrap_or(0);
        let mark = match now.cmp(&allowed) {
            std::cmp::Ordering::Greater => {
                over.push(format!("{} {}: {now} (budget {allowed})", key.0, key.1));
                "  OVER"
            }
            std::cmp::Ordering::Less => "  (can go down)",
            std::cmp::Ordering::Equal => "",
        };
        println!("{:<14} {:<36} {now:>6} {allowed:>6}{mark}", key.0, key.1);
    }

    for text in &other {
        eprint!("{text}");
    }
    if update {
        let lowered: Counts = budget
            .iter()
            .map(|(k, &allowed)| (k.clone(), counts.get(k).copied().unwrap_or(0).min(allowed)))
            .filter(|&(_, n)| n > 0)
            .collect();
        std::fs::write(&path, render_budget(&text, &lowered))
            .map_err(|e| format!("{BUDGET_FILE}: {e}"))?;
        println!("{BUDGET_FILE}: lowered to what is there now");
    }

    let mut failures = Vec::new();
    if !status.success() || errors > 0 {
        failures.push("clippy reported errors".to_owned());
    }
    if warnings > 0 {
        failures.push(format!(
            "{warnings} warning(s) outside the budget (shown above)"
        ));
    }
    if !over.is_empty() {
        failures.push(format!(
            "over the budget in {BUDGET_FILE}:\n    {}",
            over.join("\n    ")
        ));
    }
    if failures.is_empty() {
        println!("lints: within the budget");
        Ok(())
    } else {
        Err(format!("lints: {}", failures.join("\n  ")))
    }
}

/// The features of the build as shipped, on top of the default ones.
const SHIPPED: &str = "--features iai/canvas-editor-webview";

fn as_shipped(cargo_args: &[&String]) -> bool {
    cargo_args.iter().map(|a| a.as_str()).collect::<Vec<_>>()
        == SHIPPED.split(' ').collect::<Vec<_>>()
}

fn budget_path() -> Result<PathBuf, String> {
    Ok(crate::workspace_root()?.join(BUDGET_FILE))
}

fn rendered(diag: &Value) -> String {
    diag["rendered"].as_str().unwrap_or_default().to_owned()
}

/// The package's name from its id: `path+file:///…/crates/iai-raw#0.1.0`
/// names it by the folder, `path+file:///…/IAI#iai@0.1.0` after the `#`.
fn package_name(id: &str) -> String {
    let (url, fragment) = id.rsplit_once('#').unwrap_or((id, ""));
    match fragment.split_once('@') {
        Some((name, _)) => name.to_owned(),
        None => url.rsplit('/').next().unwrap_or(url).to_owned(),
    }
}

/// `crate lint count` lines; `#` starts a comment.
fn parse_budget(text: &str) -> Result<Counts, String> {
    let mut out = Counts::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let bad = || format!("{BUDGET_FILE}:{}: want `crate lint count`", i + 1);
        let mut parts = line.split_whitespace();
        let (Some(krate), Some(lint), Some(n), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(bad());
        };
        if !BUDGETED.contains(&lint) {
            return Err(format!(
                "{BUDGET_FILE}:{}: `{lint}` cannot have a budget",
                i + 1
            ));
        }
        let n = n.parse().map_err(|_| bad())?;
        out.insert((krate.to_owned(), lint.to_owned()), n);
    }
    Ok(out)
}

/// The file's leading comment, then one line per entry.
fn render_budget(old: &str, counts: &Counts) -> String {
    let mut out: String = old
        .lines()
        .take_while(|l| l.starts_with('#') || l.trim().is_empty())
        .map(|l| format!("{l}\n"))
        .collect();
    while out.ends_with("\n\n") {
        out.pop();
    }
    if !out.is_empty() {
        out.push('\n');
    }
    for ((krate, lint), n) in counts {
        out.push_str(&format!("{krate} {lint} {n}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_names_come_from_either_form_of_id() {
        assert_eq!(package_name("path+file:///C:/Users/a/IAI#iai@0.1.0"), "iai");
        assert_eq!(
            package_name("path+file:///C:/Users/a/IAI/crates/iai-raw#0.1.0"),
            "iai-raw"
        );
    }

    #[test]
    fn a_budget_reads_back_as_it_is_written() {
        let text =
            "# head\n# more\n\niai clippy::unwrap_used 3 # trailing\nxtask clippy::panic 1\n";
        let counts = parse_budget(text).unwrap();
        assert_eq!(counts.len(), 2);
        assert_eq!(
            counts[&("iai".to_owned(), "clippy::unwrap_used".to_owned())],
            3
        );
        let again = render_budget(text, &counts);
        assert!(again.starts_with("# head\n# more\n\n"));
        assert_eq!(parse_budget(&again).unwrap(), counts);
    }

    #[test]
    fn only_the_build_as_shipped_may_lower_the_budget() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let shipped = args(&["--features", "iai/canvas-editor-webview"]);
        assert!(as_shipped(&shipped.iter().collect::<Vec<_>>()));
        for other in [
            args(&[]),
            args(&["--no-default-features"]),
            args(&[
                "--features",
                "iai/canvas-editor-webview",
                "--no-default-features",
            ]),
        ] {
            assert!(!as_shipped(&other.iter().collect::<Vec<_>>()), "{other:?}");
        }
    }

    #[test]
    fn a_lint_without_a_budget_is_refused() {
        assert!(parse_budget("iai clippy::needless_return 2\n").is_err());
        assert!(parse_budget("iai clippy::unwrap_used\n").is_err());
    }
}
