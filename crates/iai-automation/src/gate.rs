//! What an agent was granted, checked before a call reaches the host: the
//! effects it may cause, the folders it may read from and write to, and the
//! sizes of what goes back and forth.
//!
//! A file path in a call (any field named `path`) is read against the
//! granted folders: a relative one is joined to the first folder of its
//! kind, an absolute one must lie inside one of them. `..`, device names
//! (`CON`, `NUL`, `COM1`…), `\\?\` and `\\.\` forms and links that lead out
//! of a folder are refused. What reaches the engine is the path made
//! absolute.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use iai_engine::{CmdError, CommandCall, CommandInfo, Effect, ErrorCode, Policy};
use serde_json::Value;

use crate::limits::{MAX_BATCH, MAX_RESULT};

/// The folders an agent may read from and write to. A folder it may write
/// to it may also read.
#[derive(Debug, Clone, Default)]
pub struct Roots {
    pub read: Vec<PathBuf>,
    pub write: Vec<PathBuf>,
}

impl Roots {
    /// Folders as given on the command line, made absolute and checked to
    /// exist.
    pub fn new(read: &[PathBuf], write: &[PathBuf]) -> Result<Self, String> {
        let fix = |dirs: &[PathBuf]| -> Result<Vec<PathBuf>, String> {
            dirs.iter()
                .map(|d| {
                    std::fs::canonicalize(d)
                        .map(|c| plain(&c))
                        .map_err(|e| format!("{}: {e}", d.display()))
                        .and_then(|c| {
                            if c.is_dir() {
                                Ok(c)
                            } else {
                                Err(format!("{} is not a folder", d.display()))
                            }
                        })
                })
                .collect()
        };
        Ok(Self {
            read: fix(read)?,
            write: fix(write)?,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.read.is_empty() && self.write.is_empty()
    }
}

/// The checks between an agent and a host.
pub struct Gate {
    policy: Policy,
    roots: Roots,
    /// Commands by id, from the host, for their effects.
    commands: BTreeMap<String, CommandInfo>,
}

fn denied(en: impl Into<String>, vi: impl Into<String>) -> CmdError {
    CmdError::new(ErrorCode::PermissionDenied, en).vi(vi)
}

/// `\\?\C:\x` as `C:\x`: what canonicalising gives on Windows, written as
/// people and file dialogs write it.
fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

/// Windows' reserved device names, which name no file in any folder.
fn is_device(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end()
        .to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ((stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.len() == 4
        && stem
            .chars()
            .last()
            .is_some_and(|c| c.is_ascii_digit() && c != '0'))
}

impl Gate {
    pub fn new(policy: Policy, roots: Roots) -> Self {
        Self {
            policy,
            roots,
            commands: BTreeMap::new(),
        }
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    pub fn roots(&self) -> &Roots {
        &self.roots
    }

    /// Learn the host's commands (for their effects).
    pub fn learn(&mut self, commands: &[CommandInfo]) {
        self.commands = commands.iter().map(|c| (c.id.clone(), c.clone())).collect();
    }

    /// Whether a command may run at all for this agent: its effects were
    /// granted, and the file ones have a folder to work in.
    pub fn allows(&self, info: &CommandInfo) -> bool {
        self.effects_check(info).is_ok()
    }

    fn effects_check(&self, info: &CommandInfo) -> Result<(), CmdError> {
        if let Some(e) = info.effects.iter().find(|e| !self.policy.grants(&[**e])) {
            let name = serde_json::to_value(e)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            let flag = match e {
                Effect::Print => " (relaunch with --allow-print)",
                Effect::Network => " (relaunch with --allow-network)",
                _ => "",
            };
            return Err(denied(
                format!(
                    "`{}` needs `{name}`, which this session was not granted{flag}",
                    info.id
                ),
                format!("Phiên này không được phép `{name}`"),
            ));
        }
        let reads = info.effects.contains(&Effect::ReadFiles);
        let writes = info.effects.contains(&Effect::WriteFiles);
        if writes && self.roots.write.is_empty() {
            return Err(denied(
                format!(
                    "`{}` writes files, and no folder to write to was granted (--write-root)",
                    info.id
                ),
                "Chưa cấp thư mục được ghi (--write-root)",
            ));
        }
        if reads && self.roots.read.is_empty() && self.roots.write.is_empty() {
            return Err(denied(
                format!(
                    "`{}` reads files, and no folder to read was granted (--read-root)",
                    info.id
                ),
                "Chưa cấp thư mục được đọc (--read-root)",
            ));
        }
        Ok(())
    }

    /// Check `call` and make its file paths absolute inside the granted
    /// folders.
    pub fn check(&self, call: &mut CommandCall) -> Result<(), CmdError> {
        let Some(info) = self.commands.get(&call.command) else {
            // The host says the command is unknown.
            return Ok(());
        };
        self.effects_check(info)?;
        let writes = info.effects.contains(&Effect::WriteFiles);
        let reads = info.effects.contains(&Effect::ReadFiles);
        if call.command == "file.save" && call.args.get("path").is_none_or(Value::is_null) {
            return Err(CmdError::bad_param(
                "path",
                "a file to write, inside a folder granted for writing",
            ));
        }
        if writes || reads {
            self.paths(&mut call.args, writes)?;
        }
        Ok(())
    }

    /// Check a batch: its size, then every call.
    pub fn check_batch(&self, calls: &mut [CommandCall]) -> Result<(), CmdError> {
        if calls.len() > MAX_BATCH {
            return Err(CmdError::new(
                ErrorCode::TooLarge,
                format!("a batch holds at most {MAX_BATCH} calls"),
            ));
        }
        calls.iter_mut().try_for_each(|c| self.check(c))
    }

    /// An answer too large to send is refused.
    pub fn check_size(&self, answer: &Value) -> Result<(), CmdError> {
        let size = serde_json::to_vec(answer).map(|v| v.len()).unwrap_or(0);
        if size > MAX_RESULT {
            return Err(CmdError::new(
                ErrorCode::TooLarge,
                format!("the answer is {size} bytes; at most {MAX_RESULT}"),
            ));
        }
        Ok(())
    }

    fn paths(&self, value: &mut Value, write: bool) -> Result<(), CmdError> {
        match value {
            Value::Object(fields) => {
                for (key, v) in fields.iter_mut() {
                    if key == "path" {
                        if let Value::String(text) = v {
                            let resolved = self.resolve(text, write)?;
                            *v = Value::String(resolved.display().to_string());
                        }
                    } else {
                        self.paths(v, write)?;
                    }
                }
                Ok(())
            }
            Value::Array(items) => items.iter_mut().try_for_each(|v| self.paths(v, write)),
            _ => Ok(()),
        }
    }

    /// `text` as an absolute path inside a granted folder.
    pub fn resolve(&self, text: &str, write: bool) -> Result<PathBuf, CmdError> {
        let refuse = |why: &str| {
            denied(
                format!("path `{text}`: {why}"),
                format!("Đường dẫn `{text}` không được phép"),
            )
        };
        if text.is_empty() || text.contains('\0') {
            return Err(refuse("empty or holds a NUL"));
        }
        if text.starts_with(r"\\?\") || text.starts_with(r"\\.\") || text.starts_with("//?/") {
            return Err(refuse("device and verbatim paths are not taken"));
        }
        let asked = Path::new(text);
        for part in asked.components() {
            match part {
                Component::ParentDir => return Err(refuse("`..` is not taken")),
                Component::Normal(name) if is_device(&name.to_string_lossy()) => {
                    return Err(refuse("a device name is not a file"))
                }
                _ => {}
            }
        }
        let roots: Vec<&PathBuf> = if write {
            self.roots.write.iter().collect()
        } else {
            self.roots
                .read
                .iter()
                .chain(self.roots.write.iter())
                .collect()
        };
        let joined = if asked.is_absolute() {
            asked.to_path_buf()
        } else {
            let Some(first) = roots.first() else {
                return Err(refuse("no folder was granted"));
            };
            first.join(asked)
        };
        // What exists is followed through its links; the rest is appended.
        let mut existing = joined.as_path();
        let mut rest = Vec::new();
        while !existing.exists() {
            match (existing.parent(), existing.file_name()) {
                (Some(parent), Some(name)) => {
                    rest.push(name.to_os_string());
                    existing = parent;
                }
                _ => return Err(refuse("lies on no disk")),
            }
        }
        let mut real = std::fs::canonicalize(existing)
            .map(|p| plain(&p))
            .map_err(|e| refuse(&e.to_string()))?;
        for name in rest.iter().rev() {
            real.push(name);
        }
        if roots.iter().any(|root| real.starts_with(root)) {
            Ok(real)
        } else {
            Err(refuse(if write {
                "it lies outside the folders granted for writing"
            } else {
                "it lies outside the folders granted"
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let d = std::env::temp_dir().join(format!("iai-gate-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn gate(read: &Path, write: &Path) -> Gate {
        let mut g = Gate::new(
            Policy::agent(),
            Roots::new(&[read.into()], &[write.into()]).unwrap(),
        );
        g.learn(&iai_engine::cmds::core_registry().list());
        g
    }

    #[test]
    fn device_names_are_spotted_with_or_without_an_extension() {
        for name in ["CON", "nul", "com1", "LPT9.txt", "aux.png", "CONOUT$"] {
            assert!(is_device(name), "{name}");
        }
        for name in ["COM0", "COMA", "console.png", "nullish", "com10"] {
            assert!(!is_device(name), "{name}");
        }
    }

    #[test]
    fn a_path_is_made_absolute_inside_the_folders_granted() {
        let (input, output) = (Dir::new("in"), Dir::new("out"));
        std::fs::write(input.0.join("a.png"), b"x").unwrap();
        let g = gate(&input.0, &output.0);
        let read = g.resolve("a.png", false).unwrap();
        assert_eq!(
            read,
            plain(&std::fs::canonicalize(input.0.join("a.png")).unwrap())
        );
        // A file to write need not exist, nor its folder.
        let written = g.resolve("new/sheet.pdf", true).unwrap();
        assert!(written.starts_with(plain(&std::fs::canonicalize(&output.0).unwrap())));
        assert!(written.ends_with("new/sheet.pdf"));
        // Absolute, inside: taken.
        let abs = output.0.join("x.png").display().to_string();
        g.resolve(&abs, true).unwrap();
        // The output folder may be read too, the input one not written.
        g.resolve(&abs, false).unwrap();
        let into_input = input.0.join("x.png").display().to_string();
        assert_eq!(
            g.resolve(&into_input, true).unwrap_err().code,
            ErrorCode::PermissionDenied
        );
    }

    #[test]
    fn escapes_and_devices_are_refused() {
        let (input, output) = (Dir::new("in2"), Dir::new("out2"));
        let g = gate(&input.0, &output.0);
        let outside = std::env::temp_dir()
            .join("elsewhere.png")
            .display()
            .to_string();
        for bad in [
            "../x.png",
            "a/../../x.png",
            r"\\?\C:\x.png",
            r"\\.\PhysicalDrive0",
            "NUL",
            "out/con.txt",
            "",
            outside.as_str(),
        ] {
            let e = g.resolve(bad, true).unwrap_err();
            assert_eq!(e.code, ErrorCode::PermissionDenied, "{bad}");
        }
    }

    #[test]
    fn a_call_has_its_paths_rewritten_and_its_effects_checked() {
        let (input, output) = (Dir::new("in3"), Dir::new("out3"));
        let g = gate(&input.0, &output.0);
        let mut call = CommandCall::new("export.pdf", json!({"path": "all.pdf"}));
        g.check(&mut call).unwrap();
        let path = call.args["path"].as_str().unwrap();
        assert!(
            Path::new(path).is_absolute() && path.ends_with("all.pdf"),
            "{path}"
        );

        let mut save = CommandCall::new("file.save", json!({}));
        assert_eq!(g.check(&mut save).unwrap_err().code, ErrorCode::BadParams);

        let mut download = CommandCall::new("ai.model_download", json!({"model": "lama"}));
        assert_eq!(
            g.check(&mut download).unwrap_err().code,
            ErrorCode::PermissionDenied
        );

        // No folder granted: no file command.
        let mut bare = Gate::new(Policy::agent(), Roots::default());
        bare.learn(&iai_engine::cmds::core_registry().list());
        let mut open = CommandCall::new("file.open", json!({"path": "a.png"}));
        let e = bare.check(&mut open).unwrap_err();
        assert!(e.message.contains("--read-root"), "{}", e.message);
    }
}
