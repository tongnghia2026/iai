//! iAi and Antigravity (Google's agent app, with Gemini): the person sends
//! an errand from the window ("Nhờ AI làm") and Gemini does it, on the
//! window, while the person watches.
//!
//! - Antigravity runs `iai.exe antigravity-relay` beside it, as a sidecar
//!   (`~/.gemini/config/sidecars/iai/sidecar.json`). Started by Antigravity,
//!   it may start conversations there (`agentapi`); it takes the errands the
//!   window holds, through the bridge while the window lets AI in, and
//!   starts a conversation for each.
//! - Gemini works through iAi's MCP server, `iai.exe mcp --prefer-window`
//!   (`~/.gemini/config/mcp_config.json`): on the open window when it lets
//!   AI in, on documents of its own when not.
//! - [`Antigravity::connect`] writes both and lets every iAi tool run
//!   unasked (the grants in Antigravity's `config.json`).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use iai_automation::bridge::{BridgeHost, Control, Errand};
use serde_json::{Map, Value, json};

/// The sidecar's id: its folder under `sidecars`, its key in `config.json`.
pub const SIDECAR: &str = "iai";
/// The relay's name, as the window hears it ask.
pub const RELAY: &str = "antigravity";
/// The command line word that runs the relay.
pub const RELAY_ARG: &str = "antigravity-relay";
/// The MCP server's name in Antigravity (its tools are `iai/<tool>`).
const SERVER: &str = "iai";

/// What is not set up yet for Antigravity to take errands from this iAi.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Missing {
    /// iAi's MCP server, on the window when it is open.
    Server,
    /// iAi's tools allowed to run unasked.
    Permissions,
    /// The relay, as a sidecar.
    Relay,
    /// The relay is there but turned off.
    RelayOff,
}

/// Antigravity's settings folder (`~/.gemini/config`).
pub struct Antigravity {
    config: PathBuf,
}

/// `a` and `b` name the same program (Windows paths ignore case).
fn same_program(a: &str, b: &Path) -> bool {
    a.eq_ignore_ascii_case(&b.display().to_string())
}

fn read_json(path: &Path) -> Result<Value, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| format!("{} không đọc được: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Write `value` whole or not at all, indented by two as Antigravity writes.
fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("Không ghi được {}: {e}", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(fail)?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("iai-tmp");
    std::fs::write(&tmp, text).map_err(fail)?;
    std::fs::rename(&tmp, path).map_err(fail)
}

/// `v[key]` as an object, made one (and `v` too) when it is not.
fn object<'a>(v: &'a mut Value, key: &str) -> Result<&'a mut Map<String, Value>, String> {
    if !v.is_object() {
        *v = json!({});
    }
    let slot = v
        .as_object_mut()
        .map(|map| map.entry(key.to_string()).or_insert_with(|| json!({})))
        .ok_or_else(|| format!("`{key}`"))?;
    if !slot.is_object() {
        *slot = json!({});
    }
    slot.as_object_mut().ok_or_else(|| format!("`{key}`"))
}

/// The grant that lets `tool` of iAi's server run unasked.
fn grant(tool: &str) -> String {
    format!("mcp({SERVER}/{tool})")
}

impl Antigravity {
    /// The user's, when there is a home folder.
    pub fn at_home() -> Option<Self> {
        let home = std::env::var_os("USERPROFILE")?;
        Some(Self::in_dir(
            PathBuf::from(home).join(".gemini").join("config"),
        ))
    }

    pub fn in_dir(config: PathBuf) -> Self {
        Self { config }
    }

    /// Antigravity has been used on this machine (it made its folder).
    pub fn installed(&self) -> bool {
        self.config.is_dir()
    }

    fn mcp_file(&self) -> PathBuf {
        self.config.join("mcp_config.json")
    }

    fn settings_file(&self) -> PathBuf {
        self.config.join("config.json")
    }

    fn sidecar_file(&self) -> PathBuf {
        self.config
            .join("sidecars")
            .join(SIDECAR)
            .join("sidecar.json")
    }

    /// What is not set up yet for `exe` and its `tools`; empty when ready.
    pub fn missing(&self, exe: &Path, tools: &[String]) -> Vec<Missing> {
        let mut missing = Vec::new();
        let mcp = read_json(&self.mcp_file()).unwrap_or_default();
        let server = mcp.pointer(&format!("/mcpServers/{SERVER}"));
        let server_ok = server.is_some_and(|s| {
            s["command"].as_str().is_some_and(|c| same_program(c, exe))
                && s["args"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|x| x == "--prefer-window"))
        });
        if !server_ok {
            missing.push(Missing::Server);
        }
        let settings = read_json(&self.settings_file()).unwrap_or_default();
        let allowed: Vec<&str> = settings
            .pointer("/userSettings/globalPermissionGrants/allow")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if tools.iter().any(|t| !allowed.contains(&grant(t).as_str())) {
            missing.push(Missing::Permissions);
        }
        let sidecar = read_json(&self.sidecar_file()).unwrap_or_default();
        let relay_ok = sidecar["command"]
            .as_str()
            .is_some_and(|c| same_program(c, exe))
            && sidecar["args"] == json!([RELAY_ARG]);
        if !relay_ok {
            missing.push(Missing::Relay);
        }
        if settings.pointer(&format!("/sidecars/{SIDECAR}/enabled")) != Some(&json!(true)) {
            missing.push(Missing::RelayOff);
        }
        missing
    }

    /// Set Antigravity up to take errands from `exe`: iAi's MCP server on the
    /// window when it is open (keeping the folders it was given, else
    /// `folder`; and reading `images`, where Antigravity's pictures land),
    /// every tool in `tools` allowed, and the relay. Whether Antigravity must
    /// start again for it to hold: it reads its permissions and MCP servers
    /// when it starts. `config.json` is kept once as it was, beside it
    /// (`config.json.bak-iai`).
    pub fn connect(
        &self,
        exe: &Path,
        tools: &[String],
        folder: &Path,
        images: Option<&Path>,
    ) -> Result<bool, String> {
        if !self.installed() {
            return Err("Máy này chưa có Antigravity (hoặc chưa mở lần nào).".to_string());
        }
        let exe_text = exe.display().to_string();

        // The MCP server: the folders it had, on the window when open.
        let mut mcp = read_json(&self.mcp_file())?;
        let servers = object(&mut mcp, "mcpServers")?;
        let before = servers.get(SERVER).cloned();
        let mut kept: Vec<String> = before
            .as_ref()
            .and_then(|s| s["args"].as_array())
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .filter(|x| !matches!(*x, "mcp" | "--bridge" | "--prefer-window"))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if !kept
            .iter()
            .any(|x| x == "--write-root" || x == "--read-root")
        {
            kept.push("--write-root".to_string());
            kept.push(folder.display().to_string());
        }
        if let Some(images) = images.map(|p| p.display().to_string()) {
            let has = kept
                .windows(2)
                .any(|w| matches!(w, [flag, dir] if flag == "--read-root" && *dir == images));
            if !has {
                kept.push("--read-root".to_string());
                kept.push(images);
            }
        }
        let mut args = vec!["mcp".to_string(), "--prefer-window".to_string()];
        args.extend(kept);
        let server = json!({"command": exe_text, "args": args});
        let server_changed = before.as_ref() != Some(&server);
        if server_changed {
            servers.insert(SERVER.to_string(), server);
            write_json(&self.mcp_file(), &mcp)?;
        }

        // Every tool allowed, and the relay turned on.
        let path = self.settings_file();
        let mut settings = read_json(&path)?;
        let backup = path.with_extension("json.bak-iai");
        if path.exists() && !backup.exists() {
            std::fs::copy(&path, &backup)
                .map_err(|e| format!("Không sao lưu được {}: {e}", path.display()))?;
        }
        let user = object(&mut settings, "userSettings")?;
        let mut grants = user
            .remove("globalPermissionGrants")
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        let mut added = false;
        if let Some(g) = grants.as_object_mut() {
            let list = g.entry("allow".to_string()).or_insert_with(|| json!([]));
            if !list.is_array() {
                *list = json!([]);
            }
            if let Some(list) = list.as_array_mut() {
                for tool in tools {
                    let g = Value::String(grant(tool));
                    if !list.contains(&g) {
                        list.push(g);
                        added = true;
                    }
                }
            }
        }
        user.insert("globalPermissionGrants".to_string(), grants);
        let relay_on =
            settings.pointer(&format!("/sidecars/{SIDECAR}/enabled")) == Some(&json!(true));
        if !relay_on {
            object(&mut settings, "sidecars")?
                .insert(SIDECAR.to_string(), json!({"enabled": true}));
        }
        if added || !relay_on {
            write_json(&path, &settings)?;
        }

        // The relay.
        let sidecar = json!({
            "command": exe_text,
            "args": [RELAY_ARG],
            "restart_policy": "always",
            "display_name": "iAi — việc từ nút «Nhờ AI làm»",
            "description": "Nhận việc người dùng bấm gửi từ app iAi và mở cuộc trò chuyện cho Gemini làm.",
        });
        if read_json(&self.sidecar_file()).ok().as_ref() != Some(&sidecar) {
            write_json(&self.sidecar_file(), &sidecar)?;
        }
        Ok(server_changed || added)
    }
}

/// Where Antigravity keeps each conversation's files, the pictures its
/// image generator draws among them (`<conversation>\<name>_<time>.jpg`).
pub fn images_folder() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE")?;
    let dir = PathBuf::from(home).join(r".gemini\antigravity\brain");
    dir.is_dir().then_some(dir)
}

/// Where Antigravity's program usually is.
fn program() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    let exe = PathBuf::from(local).join(r"Programs\Antigravity\Antigravity.exe");
    exe.exists().then_some(exe)
}

/// Antigravity's program is where it usually is (it can be started).
pub fn startable() -> bool {
    program().is_some()
}

/// Start Antigravity (it starts the relay); whether it could be.
pub fn launch() -> bool {
    // Tests never start the person's apps.
    if cfg!(test) {
        return false;
    }
    let Some(exe) = program() else {
        return false;
    };
    crate::diag::note("errand", "starting Antigravity");
    Command::new(exe).spawn().is_ok()
}

/// Stop Antigravity and start it again, so that it reads its settings
/// anew; whether it was started. What it was doing stops.
pub fn restart() -> bool {
    let Some(exe) = program().filter(|_| !cfg!(test)) else {
        return false;
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Closing its window only hides it: each of its processes started
        // from this program is stopped with what it started (its server,
        // the MCP servers, the relay); other apps' are left alone.
        let script = format!(
            "Get-Process -Name Antigravity -ErrorAction SilentlyContinue | \
             Where-Object {{ $_.Path -eq '{}' }} | \
             ForEach-Object {{ taskkill.exe /PID $_.Id /T /F | Out-Null }}",
            exe.display().to_string().replace('\'', "''")
        );
        let _ = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .creation_flags(0x0800_0000)
            .status();
    }
    let _ = exe;
    std::thread::sleep(Duration::from_millis(800));
    launch()
}

/// How agentapi is started: what Antigravity gives its sidecars, else where
/// it usually is. The language server takes `agentapi` as its first word.
fn agentapi() -> Option<(PathBuf, Vec<&'static str>)> {
    let exe = std::env::var_os("ANTIGRAVITY_AGENTAPI_EXE")
        .map(PathBuf::from)
        .or_else(|| {
            let local = std::env::var_os("LOCALAPPDATA")?;
            let exe = PathBuf::from(local)
                .join(r"Programs\Antigravity\resources\bin\language_server.exe");
            exe.exists().then_some(exe)
        })?;
    let server = exe
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("language_server.exe"));
    Some((exe, if server { vec!["agentapi"] } else { Vec::new() }))
}

/// The conversation agentapi says it started, or why it started none.
pub fn conversation_of(text: &str) -> Result<String, String> {
    let short = |t: &str| t.trim().chars().take(300).collect::<String>();
    let v: Value = serde_json::from_str(text.trim())
        .map_err(|_| format!("agentapi trả lời lạ: {}", short(text)))?;
    if let Some(id) = v
        .pointer("/response/newConversation/conversationId")
        .and_then(Value::as_str)
    {
        return Ok(id.to_string());
    }
    Err(match v.get("error").and_then(Value::as_str) {
        Some(e) => format!("Antigravity báo lỗi: {e}"),
        None => format!("agentapi trả lời lạ: {}", short(text)),
    })
}

/// The longest prompt handed on (a Windows command line holds 32767).
const MAX_PROMPT: usize = 24_000;

/// Start a conversation in Antigravity for `errand`; its id.
fn start_conversation(errand: &Errand) -> Result<String, String> {
    let (exe, lead) = agentapi().ok_or("Không tìm thấy agentapi của Antigravity trên máy này.")?;
    let mut command = Command::new(&exe);
    command.args(&lead).arg("new-conversation");
    if let Some(model) = &errand.model {
        command.arg(format!("--model={model}"));
    }
    let prompt: String = errand.prompt.chars().take(MAX_PROMPT).collect();
    command
        .arg(format!("--title={}", errand.title))
        .arg("--")
        .arg(prompt);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console window flashes up.
        command.creation_flags(0x0800_0000);
    }
    let out = command
        .output()
        .map_err(|e| format!("Không chạy được agentapi: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    conversation_of(&text).map_err(|why| {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.trim().is_empty() {
            why
        } else {
            format!(
                "{why} ({})",
                err.trim().chars().take(200).collect::<String>()
            )
        }
    })
}

/// `iai.exe antigravity-relay`: what Antigravity runs beside it until it
/// stops it. While the window lets AI in, ask it for errands now and then
/// and start a conversation for each; tell it how that went.
pub fn relay(control: &Path) -> i32 {
    crate::diag::note("relay", "waiting for errands from the window");
    let mut link: Option<BridgeHost> = None;
    // The control file as it was when it last led nowhere, and when.
    let mut tried: Option<(Option<SystemTime>, Instant)> = None;
    loop {
        if link.as_ref().is_some_and(BridgeHost::broken) {
            crate::diag::note("relay", "the window went away");
            link = None;
        }
        let Some(window) = link.as_mut() else {
            let stamp = std::fs::metadata(control).ok().map(|m| m.modified().ok());
            let again = match (&stamp, &tried) {
                (None, _) => false,
                (Some(now), Some((then, at))) => {
                    now != then || at.elapsed() >= Duration::from_secs(30)
                }
                (Some(_), None) => true,
            };
            if again {
                match Control::read(control).and_then(|c| BridgeHost::connect(&c)) {
                    Ok(window) => {
                        crate::diag::note("relay", "linked to the window");
                        link = Some(window);
                        tried = None;
                        continue;
                    }
                    Err(_) => tried = stamp.map(|s| (s, Instant::now())),
                }
            }
            std::thread::sleep(Duration::from_secs(2));
            continue;
        };
        match window.next_errand(RELAY) {
            Ok(Some(errand)) => {
                crate::diag::note("relay", &format!("errand {}: {}", errand.id, errand.title));
                // A window gone meanwhile is found broken on the next round.
                let _ = match start_conversation(&errand) {
                    Ok(conversation) => {
                        crate::diag::note("relay", &format!("conversation {conversation}"));
                        window.errand_sent(errand.id, &conversation)
                    }
                    Err(why) => {
                        crate::diag::note("relay", &format!("not handed on: {why}"));
                        window.errand_failed(errand.id, &why)
                    }
                };
            }
            // Each ask wakes the window for a frame: not too often.
            Ok(None) => std::thread::sleep(Duration::from_millis(1500)),
            Err(_) if window.broken() => {}
            // A window from before errands: ask again much later.
            Err(_) => {
                link = None;
                tried = std::fs::metadata(control)
                    .ok()
                    .map(|m| (m.modified().ok(), Instant::now()));
                std::thread::sleep(Duration::from_secs(30));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let d = std::env::temp_dir().join(format!("iai-agy-{name}-{}", std::process::id()));
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

    /// Opt-in: IAI_CONNECT_EXE is an iai.exe; the user's Antigravity is set
    /// up to take errands from it, as "Nối với Antigravity" does (it is not
    /// started again).
    #[test]
    #[ignore]
    fn probe_connect_antigravity() {
        let Ok(exe) = std::env::var("IAI_CONNECT_EXE") else {
            return;
        };
        let agy = Antigravity::at_home().unwrap();
        let tools = iai_automation::mcp::tool_names(&crate::agent::registry_for_agents().list());
        let home = PathBuf::from(std::env::var_os("USERPROFILE").unwrap());
        let again = agy
            .connect(
                Path::new(&exe),
                &tools,
                &home.join("Downloads"),
                images_folder().as_deref(),
            )
            .unwrap();
        println!("tools {} · start again: {again}", tools.len());
        assert_eq!(agy.missing(Path::new(&exe), &tools), []);
    }

    #[test]
    fn agentapis_answer_names_the_conversation_or_says_why_not() {
        let ok =
            r#"{"response": {"newConversation": {"prompt": "x", "conversationId": "4d87-c0"}}}"#;
        assert_eq!(conversation_of(ok).unwrap(), "4d87-c0");
        let no = r#"{"error": "ANTIGRAVITY_LS_ADDRESS is not set"}"#;
        assert!(
            conversation_of(no)
                .unwrap_err()
                .contains("ANTIGRAVITY_LS_ADDRESS")
        );
        assert!(conversation_of("garbage").unwrap_err().contains("lạ"));
    }

    #[test]
    fn connecting_keeps_what_was_there_and_adds_what_iai_needs() {
        let dir = Dir::new("connect");
        let agy = Antigravity::in_dir(dir.0.clone());
        let exe = PathBuf::from(r"C:\iAi\iai.exe");
        let tools = vec!["iai_file_open".to_string(), "iai_task_begin".to_string()];
        let none = Antigravity::in_dir(dir.0.join("absent"));
        assert!(!none.installed());
        assert!(
            none.connect(&exe, &tools, Path::new(r"D:\in"), None)
                .is_err()
        );

        // What the person had: another server, iAi alone on Downloads, grants.
        std::fs::write(
            dir.0.join("mcp_config.json"),
            r#"{"mcpServers": {
                "other": {"command": "x.exe"},
                "iai": {"command": "C:\\old\\iai.exe", "args": ["mcp", "--write-root", "C:\\Users\\A\\Downloads"]}
            }}"#,
        )
        .unwrap();
        std::fs::write(
            dir.0.join("config.json"),
            r#"{"userSettings": {"autoExecutionPolicy": "OFF",
                "globalPermissionGrants": {"allow": ["command(git status)", "mcp(iai/iai_file_open)"]}}}"#,
        )
        .unwrap();
        assert_eq!(
            agy.missing(&exe, &tools),
            [
                Missing::Server,
                Missing::Permissions,
                Missing::Relay,
                Missing::RelayOff
            ]
        );
        let images = Path::new(r"C:\Users\A\.gemini\antigravity\brain");
        assert!(
            agy.connect(&exe, &tools, Path::new(r"D:\in"), Some(images))
                .unwrap()
        );
        assert_eq!(agy.missing(&exe, &tools), []);
        // Antigravity's own folder names the program by the same path, case aside.
        assert_eq!(agy.missing(Path::new(r"c:\IAI\IAI.EXE"), &tools), []);

        let mcp = read_json(&dir.0.join("mcp_config.json")).unwrap();
        assert_eq!(mcp["mcpServers"]["other"]["command"], "x.exe");
        assert_eq!(
            mcp["mcpServers"]["iai"],
            json!({"command": r"C:\iAi\iai.exe",
                   "args": ["mcp", "--prefer-window", "--write-root", r"C:\Users\A\Downloads",
                            "--read-root", r"C:\Users\A\.gemini\antigravity\brain"]})
        );
        let settings = read_json(&dir.0.join("config.json")).unwrap();
        assert_eq!(settings["userSettings"]["autoExecutionPolicy"], "OFF");
        assert_eq!(
            settings["userSettings"]["globalPermissionGrants"]["allow"],
            json!([
                "command(git status)",
                "mcp(iai/iai_file_open)",
                "mcp(iai/iai_task_begin)"
            ])
        );
        assert_eq!(settings["sidecars"]["iai"], json!({"enabled": true}));
        let sidecar = read_json(&dir.0.join(r"sidecars\iai\sidecar.json")).unwrap();
        assert_eq!(sidecar["command"], r"C:\iAi\iai.exe");
        assert_eq!(sidecar["args"], json!(["antigravity-relay"]));
        assert_eq!(sidecar["restart_policy"], "always");
        // Kept once as it was.
        let kept = std::fs::read_to_string(dir.0.join("config.json.bak-iai")).unwrap();
        assert!(kept.contains("command(git status)") && !kept.contains("sidecars"));

        // Again: nothing new, nothing to start again.
        assert!(
            !agy.connect(&exe, &tools, Path::new(r"D:\in"), Some(images))
                .unwrap()
        );
        // A machine with no iAi server yet: it gets the folder offered.
        std::fs::remove_file(dir.0.join("mcp_config.json")).unwrap();
        assert!(
            agy.connect(&exe, &tools, Path::new(r"D:\in"), None)
                .unwrap()
        );
        let mcp = read_json(&dir.0.join("mcp_config.json")).unwrap();
        assert_eq!(
            mcp["mcpServers"]["iai"]["args"],
            json!(["mcp", "--prefer-window", "--write-root", r"D:\in"])
        );
    }
}
