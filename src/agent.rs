//! iAi for AI agents and scripts, with no window:
//!
//! - `iai.exe mcp [--read-root DIR]… [--write-root DIR]… [--allow-print]
//!   [--allow-network]`: a Model Context Protocol server on stdin / stdout,
//!   on documents of its own;
//! - `iai.exe mcp --bridge [--control-file FILE] …`: the same server, on the
//!   documents of the iAi window the person has open (when they let AI in,
//!   it writes where it can be reached to `%APPDATA%\IAI\control.json`);
//! - `iai.exe run SCRIPT.json [same options]`: run a script of commands
//!   (`[{"command": …, "args": …}, …]`, or `{"calls": […], "atomic": true}`)
//!   and print what each answered. A script that is not atomic may be longer
//!   than a batch: it runs a batch at a time on the same documents.
//! - `iai.exe run --skill NAME [--param NAME=VALUE]… [same options]`: run a
//!   saved skill from the library, no agent needed.
//!
//! The skill library (`--skills DIR`, `Documents\iAi_Ky_nang` by default)
//! is where `iai.exe mcp` keeps the tasks agents finish well.
//!
//! Files are read and written only inside the folders granted with
//! `--read-root` / `--write-root`. Printing and the network are off unless
//! allowed. Pro commands run when this machine's Pro is unlocked or on trial.

use std::io::Write;
use std::path::PathBuf;

use iai_automation::bridge::{BridgeHost, Control};
use iai_automation::{Gate, Host, LocalHost, Roots};
use iai_engine::{CommandCall, Effect, Engine, Policy, Source};

const USAGE: &str = "\
iAi for AI agents and scripts (no window)

  iai.exe mcp [options]            MCP server on stdin/stdout
  iai.exe mcp --bridge [options]   MCP server on the iAi window that is open
  iai.exe run SCRIPT.json [options]
  iai.exe run --skill NAME [--param NAME=VALUE]... [options]

options:
  --read-root DIR       a folder files may be read from (repeat for more)
  --write-root DIR      a folder files may be written to (and read from)
  --allow-print         let commands print (paper and ink)
  --allow-network       let commands use the network (model downloads)
  --dry-run             with --skill: check its params and commands, run nothing
  --skills DIR          the skill library (default Documents\\iAi_Ky_nang)
  --control-file FILE   where the open window says how to reach it
                        (default %APPDATA%\\IAI\\control.json)
";

/// What the command line asked for.
struct Options {
    read: Vec<PathBuf>,
    write: Vec<PathBuf>,
    print: bool,
    network: bool,
    bridge: bool,
    control: Option<PathBuf>,
    script: Option<PathBuf>,
    skills: Option<PathBuf>,
    skill: Option<String>,
    params: Vec<(String, String)>,
    dry_run: bool,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut o = Options {
        read: Vec::new(),
        write: Vec::new(),
        print: false,
        network: false,
        bridge: false,
        control: None,
        script: None,
        skills: None,
        skill: None,
        params: Vec::new(),
        dry_run: false,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .map(PathBuf::from)
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "--read-root" => o.read.push(value("--read-root")?),
            "--write-root" => o.write.push(value("--write-root")?),
            "--control-file" => o.control = Some(value("--control-file")?),
            "--skills" => o.skills = Some(value("--skills")?),
            "--skill" => o.skill = Some(value("--skill")?.display().to_string()),
            "--param" => {
                let pair = value("--param")?.display().to_string();
                let (k, v) = pair
                    .split_once('=')
                    .ok_or_else(|| format!("--param takes NAME=VALUE, not {pair}"))?;
                o.params.push((k.trim().to_string(), v.to_string()));
            }
            "--allow-print" => o.print = true,
            "--allow-network" => o.network = true,
            "--dry-run" => o.dry_run = true,
            "--bridge" => o.bridge = true,
            "-h" | "--help" => return Err(String::new()),
            other if !other.starts_with('-') && o.script.is_none() => {
                o.script = Some(PathBuf::from(other));
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(o)
}

/// Where the open window says how to reach it.
pub fn control_file() -> PathBuf {
    crate::diag::data_dir().join("control.json")
}

/// The commands an agent may run: the free core's, and the Pro pack's,
/// which say Pro is locked when this machine's Pro is not open.
pub fn registry_for_agents() -> iai_engine::Registry {
    #[cfg_attr(not(feature = "pro"), allow(unused_mut))]
    let mut registry = iai_engine::cmds::core_registry();
    #[cfg(feature = "pro")]
    if crate::pro::Pro::load().status().open() {
        crate::pro::cmds::register(&mut registry);
    } else {
        crate::pro::cmds::register_locked(&mut registry);
    }
    registry
}

fn policy(o: &Options) -> Policy {
    let mut p = Policy::agent();
    if o.print {
        p = p.grant(Effect::Print);
    }
    if o.network {
        p = p.grant(Effect::Network);
    }
    p
}

fn version() -> String {
    let build = crate::update::this_build();
    format!("{} ({})", build.commit, build.day())
}

/// Run `iai.exe mcp` / `iai.exe run` when the command line asks for one;
/// the exit code, or `None` to start the window.
pub fn run_if_asked() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mode, rest) = args.split_first()?;
    if mode != "mcp" && mode != "run" {
        return None;
    }
    let options = match parse(rest) {
        Ok(o) => o,
        Err(why) => {
            if !why.is_empty() {
                eprintln!("iai: {why}");
            }
            eprint!("{USAGE}");
            return Some(2);
        }
    };
    crate::diag::start_without_window(&version(), &format!("agent: iai {}", args.join(" ")));
    let roots = match Roots::new(&options.read, &options.write) {
        Ok(r) => r,
        Err(why) => {
            eprintln!("iai: {why}");
            return Some(2);
        }
    };
    let gate = Gate::new(policy(&options), roots);
    let mut host: Box<dyn Host> = if options.bridge {
        let file = options.control.clone().unwrap_or_else(control_file);
        let control = match Control::read(&file) {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "iai: no open iAi window to reach ({}: {e}); open iAi and let AI in first",
                    file.display()
                );
                return Some(3);
            }
        };
        match BridgeHost::connect(&control) {
            Ok(h) => Box::new(h),
            Err(e) => {
                eprintln!("iai: could not reach the open iAi window: {e}");
                return Some(3);
            }
        }
    } else {
        let engine = Engine::new(registry_for_agents(), Source::Agent, policy(&options));
        Box::new(LocalHost::new(engine))
    };
    let library = iai_automation::skills::Store::new(
        options
            .skills
            .clone()
            .unwrap_or_else(iai_automation::skills::default_root),
    );
    let code = match (mode.as_str(), &options.skill) {
        ("mcp", _) => serve(host.as_mut(), gate, library),
        (_, Some(name)) => run_skill(
            host.as_mut(),
            gate,
            &library,
            name,
            &options.params,
            options.dry_run,
        ),
        _ => run_script(host.as_mut(), gate, options.script.as_deref()),
    };
    crate::diag::finish();
    Some(code)
}

fn serve(host: &mut dyn Host, gate: Gate, library: iai_automation::skills::Store) -> i32 {
    let mut server = match iai_automation::mcp::Server::new(host, gate, &version()) {
        Ok(s) => s.with_skills(library),
        Err(e) => {
            eprintln!("iai: {}", e.message);
            return 1;
        }
    };
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    match server.serve(stdin.lock(), stdout.lock()) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("iai: {e}");
            1
        }
    }
}

/// `--param` text as a value: JSON when it reads as JSON (numbers, true,
/// lists), else the text itself.
fn param_value(text: &str) -> serde_json::Value {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(v) if !v.is_string() => v,
        _ => serde_json::Value::String(text.to_string()),
    }
}

/// `iai.exe run --skill`: a saved skill from start to end, no agent.
fn run_skill(
    host: &mut dyn Host,
    mut gate: Gate,
    library: &iai_automation::skills::Store,
    name: &str,
    params: &[(String, String)],
    dry_run: bool,
) -> i32 {
    match host.commands() {
        Ok(list) => gate.learn(&list),
        Err(e) => {
            eprintln!("iai: {}", e.message);
            return 1;
        }
    }
    let given: serde_json::Map<String, serde_json::Value> = params
        .iter()
        .map(|(k, v)| (k.clone(), param_value(v)))
        .collect();
    let given = serde_json::Value::Object(given);
    let (mut run, plan) = match iai_automation::skills::Run::start(library, &gate, name, &given) {
        Ok(r) => r,
        Err(e) => {
            println!("{}", serde_json::json!({"failed": {"error": e}}));
            return 1;
        }
    };
    // Its SKILL.md, written if a recipe came without one.
    let _ = library.skill_md(name);
    if dry_run {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"status": "checked", "plan": plan}))
                .unwrap_or_default()
        );
        return 0;
    }
    match run.advance(host, &gate, library, None) {
        Ok(_) => {
            library.note(
                name,
                &format!("chạy · xong · {} lệnh · iai.exe run", run.calls),
            );
            let next = plan
                .get("howToRun")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "status": "done", "skill": name, "calls": run.calls,
                    "written": run.written, "howToRun": next,
                }))
                .unwrap_or_default()
            );
            0
        }
        Err(f) => {
            let broken = matches!(
                f.error.code,
                iai_engine::ErrorCode::BadParams | iai_engine::ErrorCode::NotFound
            );
            let _ = library.record_use(name, false, broken, "", "iai.exe run");
            println!("{}", serde_json::json!({"failed": f.to_json()}));
            1
        }
    }
}

fn run_script(host: &mut dyn Host, mut gate: Gate, script: Option<&std::path::Path>) -> i32 {
    let Some(path) = script else {
        eprintln!("iai: run needs a script file");
        eprint!("{USAGE}");
        return 2;
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("iai: {}: {e}", path.display());
            return 2;
        }
    };
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("iai: {}: {e}", path.display());
            return 2;
        }
    };
    let (calls, atomic) = match &value {
        serde_json::Value::Array(_) => (value.clone(), false),
        _ => (
            value.get("calls").cloned().unwrap_or_default(),
            value
                .get("atomic")
                .and_then(|a| a.as_bool())
                .unwrap_or(false),
        ),
    };
    let calls: Vec<CommandCall> = match serde_json::from_value(calls) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("iai: {}: the calls do not read: {e}", path.display());
            return 2;
        }
    };
    match host.commands() {
        Ok(list) => gate.learn(&list),
        Err(e) => {
            eprintln!("iai: {}", e.message);
            return 1;
        }
    }
    let reply = match iai_automation::script::run(host, &gate, calls, atomic) {
        Ok(r) => r,
        Err(e) => {
            println!(
                "{}",
                serde_json::json!({"failed": {"index": null, "error": e}})
            );
            return 1;
        }
    };
    let failed = reply.failed.is_some();
    let mut out = std::io::stdout().lock();
    let _ = writeln!(
        out,
        "{}",
        serde_json::to_string_pretty(&reply).unwrap_or_default()
    );
    // A script's long commands ran to their end inside the batch.
    i32::from(failed)
}

#[cfg(feature = "pro")]
#[cfg(test)]
#[rustfmt::skip]
mod e2e;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_reads_its_folders_and_permissions() {
        let args: Vec<String> = [
            "--read-root",
            "in",
            "--write-root",
            "out",
            "--read-root",
            "in2",
            "--allow-print",
            "script.json",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let o = parse(&args).unwrap();
        assert_eq!(o.read, [PathBuf::from("in"), PathBuf::from("in2")]);
        assert!(o.skill.is_none() && o.params.is_empty());
        assert_eq!(o.write, [PathBuf::from("out")]);
        assert!(o.print && !o.network && !o.bridge);
        assert_eq!(o.script, Some(PathBuf::from("script.json")));
        assert!(policy(&o).grants(&[Effect::Print]));
        assert!(!policy(&o).grants(&[Effect::Network]));
        assert!(parse(&["--read-root".to_string()]).is_err());
        assert!(parse(&["--bogus".to_string()]).is_err());

        let skill: Vec<String> = [
            "--skill", "anh-the", "--param", "co=2x3", "--param", "so_ban=4", "--skills", "kho",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let o = parse(&skill).unwrap();
        assert_eq!(o.skill.as_deref(), Some("anh-the"));
        assert_eq!(
            o.params,
            [
                ("co".to_string(), "2x3".to_string()),
                ("so_ban".to_string(), "4".to_string())
            ]
        );
        assert_eq!(o.skills, Some(PathBuf::from("kho")));
        assert_eq!(param_value("4"), serde_json::json!(4));
        assert_eq!(param_value("2x3"), serde_json::json!("2x3"));
        assert!(parse(&["--param".to_string(), "no-equals".to_string()]).is_err());
    }
}
