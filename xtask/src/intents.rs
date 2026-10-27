//! What the window still asks for outside the command engine.
//!
//! Every field of the intent structs in `src/ui/intent.rs` (`UiActions` and
//! the groups it holds) is something a menu, panel or dialog asks the app
//! for directly. Phase 5 turns them into engine commands one by one (asked
//! for through `UiActions::commands`), until none is left. This check counts
//! them per struct and fails when a struct has more than
//! `xtask/intent-budget.txt` allows; `--update` lowers the numbers after
//! fields are taken out, never raises them. `--list` prints every field
//! still there: the list of what has no command yet.

use std::collections::BTreeMap;
use std::path::PathBuf;

const BUDGET_FILE: &str = "xtask/intent-budget.txt";
const INTENT_FILE: &str = "src/ui/intent.rs";

/// The structs whose fields are asks: `UiActions` and its groups.
const STRUCTS: &[&str] = &[
    "UiActions",
    "DocumentIntent",
    "LayerIntent",
    "ToolIntent",
    "SelectionIntent",
    "DevelopIntent",
    "PrintIntent",
    "DialogIntent",
    "ChromeIntent",
    "LibraryIntent",
    "ChannelsIntent",
    "AiIntent",
    "SettingsIntent",
];

/// `UiActions` fields that are not asks: the groups, and the commands.
const NOT_ASKS: &[&str] = &[
    "commands", "doc", "layers", "tool", "sel", "develop", "print", "dialogs", "chrome", "library",
    "channels", "ai", "settings",
];

/// Struct → its ask fields, in the order written.
type Fields = BTreeMap<String, Vec<String>>;

pub fn run(args: Vec<String>) -> Result<(), String> {
    let update = args.iter().any(|a| a == "--update");
    let list = args.iter().any(|a| a == "--list");
    let root = crate::workspace_root()?;
    let source = std::fs::read_to_string(root.join(INTENT_FILE))
        .map_err(|e| format!("{INTENT_FILE}: {e}"))?;
    let fields = ask_fields(&source);
    if list {
        for (name, names) in &fields {
            for field in names {
                println!("{name}.{field}");
            }
        }
    }
    let counts: BTreeMap<String, u64> = fields
        .iter()
        .map(|(name, names)| (name.clone(), names.len() as u64))
        .collect();
    let total: u64 = counts.values().sum();

    let path = budget_path()?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{BUDGET_FILE}: {e}"))?;
    let budget = parse_budget(&text)?;

    let mut over = Vec::new();
    for (name, &n) in &counts {
        let allowed = budget.get(name).copied().unwrap_or(0);
        if n > allowed {
            over.push(format!(
                "{name}: {n} asks, the budget allows {allowed} — ask for an engine command \
                 (`UiActions::run`) instead of adding a field"
            ));
        }
    }
    if update {
        if !over.is_empty() {
            return Err(format!(
                "intents: --update only lowers the budget\n{}",
                over.join("\n")
            ));
        }
        std::fs::write(&path, render_budget(&text, &counts))
            .map_err(|e| format!("{BUDGET_FILE}: {e}"))?;
        println!("intents: {total} asks left; budget written");
        return Ok(());
    }
    if !over.is_empty() {
        return Err(format!("intents: over the budget\n{}", over.join("\n")));
    }
    let budget_total: u64 = budget.values().sum();
    if total < budget_total {
        println!(
            "intents: {total} asks left (budget {budget_total}): lower it with \
             `cargo xtask intents --update`"
        );
    } else {
        println!("intents: {total} asks left, within the budget");
    }
    Ok(())
}

fn budget_path() -> Result<PathBuf, String> {
    Ok(crate::workspace_root()?.join(BUDGET_FILE))
}

/// The ask fields of every struct in [`STRUCTS`] found in `source`.
fn ask_fields(source: &str) -> Fields {
    let mut out = Fields::new();
    let mut current: Option<String> = None;
    for line in source.lines() {
        if let Some(name) = current.as_ref() {
            if line.starts_with('}') {
                current = None;
                continue;
            }
            let Some(rest) = line.strip_prefix("    pub ") else {
                continue;
            };
            let Some((field, _)) = rest.split_once(':') else {
                continue;
            };
            let field = field.trim();
            if name == "UiActions" && NOT_ASKS.contains(&field) {
                continue;
            }
            out.entry(name.clone()).or_default().push(field.to_string());
            continue;
        }
        let Some(rest) = line.strip_prefix("pub struct ") else {
            continue;
        };
        let name = rest.trim_end_matches('{').trim();
        if STRUCTS.contains(&name) && line.trim_end().ends_with('{') {
            out.entry(name.to_string()).or_default();
            current = Some(name.to_string());
        }
    }
    out
}

fn parse_budget(text: &str) -> Result<BTreeMap<String, u64>, String> {
    let mut out = BTreeMap::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(name), Some(n), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(format!(
                "{BUDGET_FILE}:{}: expected `struct count`",
                i.saturating_add(1)
            ));
        };
        let n = n
            .parse::<u64>()
            .map_err(|e| format!("{BUDGET_FILE}:{}: {e}", i.saturating_add(1)))?;
        out.insert(name.to_string(), n);
    }
    Ok(out)
}

/// `old` with its comment lines kept and the counts written anew.
fn render_budget(old: &str, counts: &BTreeMap<String, u64>) -> String {
    let mut out: String = old
        .lines()
        .take_while(|l| l.trim().is_empty() || l.starts_with('#'))
        .map(|l| format!("{l}\n"))
        .collect();
    for (name, n) in counts {
        out.push_str(&format!("{name} {n}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "\
#[derive(Default)]
pub struct DocumentIntent {
    /// Open a file.
    pub open_file: bool,
    pub zoom_in: bool,
    #[cfg(feature = \"pro\")]
    pub impose_sheet: Option<(u32,
        u32)>,
}

pub struct Unrelated {
    pub ignored: bool,
}

#[derive(Default)]
pub struct UiActions {
    pub commands: Vec<u32>,
    pub doc: DocumentIntent,
    pub exit_now: bool,
}
";

    #[test]
    fn the_asks_are_the_fields_of_the_intent_structs() {
        let fields = ask_fields(SOURCE);
        assert_eq!(
            fields.get("DocumentIntent").map(Vec::len),
            Some(3),
            "{fields:?}"
        );
        assert_eq!(fields.get("UiActions"), Some(&vec!["exit_now".to_string()]));
        assert!(!fields.contains_key("Unrelated"));
    }

    #[test]
    fn a_budget_reads_back_as_it_is_written() {
        let old = "# note\n\nDocumentIntent 9\n";
        let counts: BTreeMap<String, u64> = [("DocumentIntent".to_string(), 3)].into();
        let text = render_budget(old, &counts);
        assert!(text.starts_with("# note\n"));
        assert_eq!(parse_budget(&text).unwrap(), counts);
        assert!(parse_budget("DocumentIntent").is_err());
    }

    /// The real file parses, and the groups are counted.
    #[test]
    fn the_intents_of_the_app_are_counted() {
        let root = crate::workspace_root().unwrap();
        let source = std::fs::read_to_string(root.join(INTENT_FILE)).unwrap();
        let fields = ask_fields(&source);
        for name in STRUCTS {
            assert!(fields.contains_key(*name), "{name}");
        }
        assert!(fields["DocumentIntent"].iter().any(|f| f == "open_file"));
        assert!(!fields["UiActions"].iter().any(|f| f == "commands"));
    }
}
