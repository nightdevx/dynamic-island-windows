//! Claude Code sessions and plan limits. Claude Code runs `island.exe --hook` and
//! `island.exe --statusline`; those write small files the island watches. Nothing polls.

use std::{
    collections::HashMap,
    io::Read,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use windows::{
    core::*,
    Data::Json::JsonObject,
    Win32::{
        Foundation::*,
        System::{Diagnostics::ToolHelp::*, Threading::*},
    },
};

use crate::{json, text::tr};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum State {
    Open,
    Working,
    Permission,
    Done,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Open => tr("açık", "open"),
            State::Working => tr("çalışıyor", "working"),
            State::Permission => tr("izin bekliyor", "needs permission"),
            State::Done => tr("yanıt hazır", "answer ready"),
        }
    }

    pub fn color(self) -> u32 {
        match self {
            State::Open => 0xFF8E8E93,
            State::Working => 0xFF0A84FF,
            State::Permission => 0xFFFF9F0A,
            State::Done => 0xFF30D158,
        }
    }

    fn name(self) -> &'static str {
        match self {
            State::Open => "open",
            State::Working => "working",
            State::Permission => "permission",
            State::Done => "done",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [State::Open, State::Working, State::Permission, State::Done].into_iter().find(|s| s.name() == text)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub id: String,
    pub project: String,
    pub state: State,
    pub updated_ms: u64,
    pub pid: u32, // the Claude Code process; 0 if unknown
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub used_percentage: f64,
    pub resets_at: u64, // unix seconds
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Limits {
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
    pub updated: u64, // unix seconds
}

pub fn dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("DynamicIsland").join("claude"))
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// What a hook event means for the session; None leaves it as it is.
enum Change {
    To(State),
    End,
}

fn change(event: &str, notification_type: Option<&str>) -> Option<Change> {
    Some(match (event, notification_type) {
        ("SessionStart", _) => Change::To(State::Open),
        ("UserPromptSubmit" | "PostToolUse", _) => Change::To(State::Working),
        ("PermissionRequest", _) | ("Notification", Some("permission_prompt")) => Change::To(State::Permission),
        ("Stop", _) => Change::To(State::Done),
        ("SessionEnd", _) => Change::End,
        _ => return None,
    })
}

// ---- Claude Code side: short-lived processes started by hooks and the status line ----

/// `island.exe --hook`: records the session's state. Never fails the hook: errors go to the log.
pub fn hook() {
    if let Err(e) = record_hook(&stdin()) {
        crate::log(format!("hook: {e}"));
    }
}

fn record_hook(input: &str) -> Result<()> {
    let json = JsonObject::Parse(&HSTRING::from(input))?;
    let (Some(event), Some(id)) = (string(&json, "hook_event_name"), string(&json, "session_id")) else {
        return Ok(());
    };
    // Session ids are UUIDs; anything else must not become part of a path.
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Ok(());
    }
    let Some(change) = change(&event, string(&json, "notification_type").as_deref()) else { return Ok(()) };
    let Some(dir) = dir() else { return Ok(()) };
    let path = dir.join(format!("session-{id}.txt"));
    match change {
        Change::End => {
            let _ = std::fs::remove_file(&path);
        }
        Change::To(state) => {
            let now = now_ms();
            let previous = std::fs::read_to_string(&path).map(|text| fields(&text)).unwrap_or_default();
            // Async hooks can finish out of order: an older event must not overwrite a newer one.
            if previous.get("updated").and_then(|v| v.parse::<u64>().ok()).is_some_and(|t| t > now) {
                return Ok(());
            }
            // PostToolUse fires on every tool call; rewriting the same state would wake the island for nothing.
            if previous.get("state").map(String::as_str) == Some(state.name()) {
                return Ok(());
            }
            let pid = previous.get("pid").cloned().unwrap_or_else(|| claude_pid().to_string());
            let cwd = string(&json, "cwd").unwrap_or_default();
            let text = format!("state={}\ncwd={cwd}\npid={pid}\nupdated={now}\n", state.name());
            write_atomic(&path, &text)?;
        }
    }
    Ok(())
}

/// `island.exe --statusline`: saves the plan limits and prints the status line text.
pub fn statusline() {
    let text = match record_statusline(&stdin()) {
        Ok(text) => text,
        Err(e) => {
            crate::log(format!("statusline: {e}"));
            String::new()
        }
    };
    print!("{text}");
}

fn record_statusline(input: &str) -> Result<String> {
    let json = JsonObject::Parse(&HSTRING::from(input))?;
    let model = object(&json, "model").and_then(|m| string(&m, "display_name")).unwrap_or_default();
    let limits = object(&json, "rate_limits").map(|r| Limits {
        five_hour: window(&r, "five_hour"),
        seven_day: window(&r, "seven_day"),
        updated: now_ms() / 1000,
    });
    if let (Some(limits), Some(dir)) = (&limits, dir()) {
        save_limits(&dir.join("limits.txt"), limits)?;
    }
    Ok(status_text(&model, limits.as_ref()))
}

fn status_text(model: &str, limits: Option<&Limits>) -> String {
    let mut parts = vec![model.to_string()];
    if let Some(limits) = limits {
        for (label, window) in [(tr("5 sa", "5h"), limits.five_hour), (tr("hafta", "week"), limits.seven_day)] {
            if let Some(w) = window {
                let used = w.used_percentage.clamp(0.0, 100.0);
                parts.push(tr(format!("{label} %{used:.0} kullanıldı"), format!("{label} {used:.0}% used")));
            }
        }
    }
    parts.retain(|p| !p.is_empty());
    parts.join(" · ")
}

/// The status line runs every few hundred ms while Claude streams; write only when the
/// numbers change, or every 5 minutes to keep the "updated" time honest.
pub fn save_limits(path: &std::path::Path, limits: &Limits) -> Result<()> {
    let old = std::fs::read_to_string(path).ok().map(|text| parse_limits(&text));
    if let Some(old) = old {
        let same = old.five_hour == limits.five_hour && old.seven_day == limits.seven_day;
        if same && limits.updated < old.updated + 300 {
            return Ok(());
        }
    }
    let line = |w: Option<Window>| w.map(|w| format!("{} {}", w.used_percentage, w.resets_at)).unwrap_or_default();
    let text = format!(
        "five_hour={}\nseven_day={}\nupdated={}\n",
        line(limits.five_hour),
        line(limits.seven_day),
        limits.updated
    );
    write_atomic(path, &text)
}

fn stdin() -> String {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    // Some writers (Windows PowerShell among them) start with a BOM, which the JSON parser rejects.
    input.trim_start_matches('\u{feff}').to_string()
}

/// Readers never see a half-written file.
fn write_atomic(path: &std::path::Path, text: &str) -> Result<()> {
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::create_dir_all(path.parent().unwrap_or(path))
        .and_then(|_| std::fs::write(&temp, text))
        .and_then(|_| std::fs::rename(&temp, path))
        .map_err(|e| Error::new(E_FAIL, e.to_string()))
}

/// Every running process: pid → (parent pid, lowercase exe name).
pub fn processes() -> HashMap<u32, (u32, String)> {
    let mut parents = HashMap::new();
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return parents };
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase();
            parents.insert(entry.th32ProcessID, (entry.th32ParentProcessID, name));
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
    parents
}

/// The Claude Code process that started this hook (claude.exe, or node.exe for npm
/// installs), so the island can drop sessions whose process is gone. 0 if not found.
fn claude_pid() -> u32 {
    let parents = processes();
    let mut pid = std::process::id();
    for _ in 0..8 {
        let Some((parent, _)) = parents.get(&pid) else { break };
        pid = *parent;
        match parents.get(&pid) {
            Some((_, name)) if name == "claude.exe" || name == "node.exe" => return pid,
            Some(_) => {}
            None => break,
        }
    }
    0
}

// ---- Installer side: Claude Code's settings ----

/// The hooks the island needs: (event, matcher, waited for). Starting and ending a session
/// wait for the hook (it is quick); the rest run in the background.
const HOOKS: [(&str, Option<&str>, bool); 7] = [
    ("SessionStart", None, true),
    ("UserPromptSubmit", None, false),
    ("PostToolUse", None, false),
    ("PermissionRequest", None, false),
    ("Notification", Some("permission_prompt"), false),
    ("Stop", None, false),
    ("SessionEnd", None, true),
];

/// `island.exe --setup-claude` (`add`) and `--remove-claude`: adds or removes the island's
/// hooks and status line in `~/.claude/settings.json`. Run by the installer and uninstaller;
/// nothing happens without Claude Code. Never fails the caller: the outcome goes to the log.
pub fn setup(add: bool) {
    let outcome = (|| {
        let home = std::env::var_os("USERPROFILE").ok_or("no USERPROFILE")?;
        let dir = PathBuf::from(home).join(".claude");
        if !dir.is_dir() {
            return Ok("Claude Code is not installed".to_string());
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        edit_settings(&dir.join("settings.json"), &exe.display().to_string().replace('\\', "/"), add)
    })();
    crate::log(format!("claude setup: {}", outcome.unwrap_or_else(|e: String| format!("failed: {e}"))));
}

/// Whether Claude Code reports to the island; None when Claude Code is not installed.
pub fn connected() -> Option<bool> {
    let dir = PathBuf::from(std::env::var_os("USERPROFILE")?).join(".claude");
    if !dir.is_dir() {
        return None;
    }
    let text = std::fs::read_to_string(dir.join("settings.json")).unwrap_or_default();
    let root = json::parse(&text);
    Some(root.is_some_and(|root| {
        let mut without = root.clone();
        remove_island(&mut without);
        without != root || is_island(root.get("statusLine").and_then(|s| s.get("command")))
    }))
}

/// Edits settings.json in place: other keys, hooks and formatting stay as they are. The
/// first edit keeps a copy of the file as it was. Returns what it did.
fn edit_settings(path: &std::path::Path, exe: &str, add: bool) -> std::result::Result<String, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) if add => "{}".to_string(),
        Err(_) => return Ok("no settings.json".to_string()),
    };
    let mut root = json::parse(&text).ok_or("settings.json is not valid JSON; left as it is")?;
    if !matches!(root, json::Value::Object(_)) {
        return Err("settings.json is not an object".into());
    }
    let before = root.clone();
    remove_island(&mut root);
    if add {
        add_island(&mut root, exe)?;
    } else {
        if is_island(root.get("statusLine").and_then(|s| s.get("command"))) {
            root.remove("statusLine");
        }
        if matches!(root.get("hooks"), Some(json::Value::Object(events)) if events.is_empty()) {
            root.remove("hooks");
        }
    }
    if root == before {
        return Ok("already as wanted".into());
    }
    let backup = path.with_extension("json.bak-island");
    if path.exists() && !backup.exists() {
        let _ = std::fs::copy(path, &backup);
    }
    let ending = if text.ends_with('\n') { "\n" } else { "" };
    write_atomic(path, &(json::print(&root) + ending)).map_err(|e| e.to_string())?;
    Ok(if add { "hooks added" } else { "hooks removed" }.into())
}

/// A command that runs the island, from wherever it is installed.
fn is_island(command: Option<&json::Value>) -> bool {
    command.and_then(json::Value::raw).is_some_and(|c| {
        let c = c.to_lowercase();
        c.contains("dynamicisland") && c.contains("island.exe")
    })
}

/// Takes the island's hooks out, with the groups and events they leave empty.
fn remove_island(root: &mut json::Value) {
    let Some(json::Value::Object(events)) = root.get_mut("hooks") else { return };
    for (_, groups) in events.iter_mut() {
        if let json::Value::Array(groups) = groups {
            for group in groups.iter_mut() {
                if let Some(json::Value::Array(hooks)) = group.get_mut("hooks") {
                    hooks.retain(|hook| !is_island(hook.get("command")));
                }
            }
            groups.retain(|group| !matches!(group.get("hooks"), Some(json::Value::Array(hooks)) if hooks.is_empty()));
        }
    }
    events.retain(|(_, groups)| !matches!(groups, json::Value::Array(groups) if groups.is_empty()));
}

/// Adds a group with the island's hook to each event it needs, after any others, and the
/// status line unless another one is set.
fn add_island(root: &mut json::Value, exe: &str) -> std::result::Result<(), String> {
    use json::Value;
    if root.get("hooks").is_none() {
        root.set("hooks", Value::Object(Vec::new()));
    }
    let Some(Value::Object(events)) = root.get_mut("hooks") else { return Err("\"hooks\" is not an object".into()) };
    for (event, matcher, waited) in HOOKS {
        let wait = if waited { ("timeout", Value::Raw("5".into())) } else { ("async", Value::Raw("true".into())) };
        let hook = Value::object(vec![
            ("type", Value::string("command")),
            ("command", Value::string(exe)),
            ("args", Value::Array(vec![Value::string("--hook")])),
            wait,
        ]);
        let mut group: Vec<(&str, Value)> = matcher.map(|m| ("matcher", Value::string(m))).into_iter().collect();
        group.push(("hooks", Value::Array(vec![hook])));
        match events.iter_mut().find(|(name, _)| name == event) {
            Some((_, Value::Array(groups))) => groups.push(Value::object(group)),
            Some(_) => return Err(format!("hooks.{event} is not a list")),
            None => events.push((event.to_string(), Value::Array(vec![Value::object(group)]))),
        }
    }
    let status = root.get("statusLine");
    if status.is_none() || is_island(status.and_then(|s| s.get("command"))) {
        let command = if exe.contains(' ') { format!("\"{exe}\" --statusline") } else { format!("{exe} --statusline") };
        root.set("statusLine", Value::object(vec![("type", Value::string("command")), ("command", Value::string(&command))]));
    }
    Ok(())
}

// ---- Island side ----

/// The closed island's Claude line: a status, what it is about, and the color of the mark.
/// The session that needs the user most, or the ones working, or what is open. `unseen`
/// tells which waiting sessions the user has not looked at yet.
pub fn summary(sessions: &[Session], unseen: impl Fn(&Session) -> bool) -> (String, String, u32) {
    let name = |s: &Session| if s.project.is_empty() { "Claude".to_string() } else { s.project.clone() };
    // "web", or "web +2" when two more sessions are open.
    let about = |s: &Session| match sessions.len() {
        0 | 1 => name(s),
        n => format!("{} +{}", name(s), n - 1),
    };
    let line = |title: &str, detail: String, state: State| (title.to_string(), detail, state.color());
    if let Some(s) = sessions.iter().find(|s| s.state == State::Permission) {
        return line(tr("İzin bekliyor", "Needs permission"), about(s), State::Permission);
    }
    if let Some(s) = sessions.iter().find(|s| s.state == State::Done && unseen(s)) {
        return line(tr("Yanıt hazır", "Answer ready"), about(s), State::Done);
    }
    let working: Vec<&Session> = sessions.iter().filter(|s| s.state == State::Working).collect();
    match (working.len(), sessions.len()) {
        (1, _) => line(tr("Çalışıyor", "Working"), about(working[0]), State::Working),
        (n, _) if n > 1 => line(tr("Çalışıyor", "Working"), tr(format!("{n} oturum"), format!("{n} sessions")), State::Working),
        (_, 0) => line("Claude", tr("Açık oturum yok", "No open sessions").into(), State::Open),
        (_, 1) => line("Claude", name(&sessions[0]), State::Open),
        (_, n) => line("Claude", tr(format!("{n} oturum açık"), format!("{n} sessions open")), State::Open),
    }
}

/// Percent used of the 5-hour limit (0 once it has reset), and its color.
pub fn five_hour_used(limits: &Limits, now: u64) -> Option<(f32, u32)> {
    let w = limits.five_hour?;
    let used = if now >= w.resets_at { 0.0 } else { w.used_percentage.clamp(0.0, 100.0) as f32 };
    Some((used, usage_color(used)))
}

/// A limit's color by how much of it is used: Claude's own, orange from 70%, red from 90%.
pub fn usage_color(used: f32) -> u32 {
    if used >= 90.0 {
        0xFFFF453A
    } else if used >= 70.0 {
        0xFFFF9F0A
    } else {
        0xFFD97757
    }
}

/// Sessions whose Claude process still runs, newest first, and the last known limits.
pub fn load() -> (Vec<Session>, Limits) {
    let Some(dir) = dir() else { return (Vec::new(), Limits::default()) };
    let mut sessions = Vec::new();
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(id) = name.strip_prefix("session-").and_then(|n| n.strip_suffix(".txt")) else { continue };
        let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
        let f = fields(&text);
        let pid = f.get("pid").and_then(|p| p.parse().ok()).unwrap_or(0);
        if pid != 0 && !alive(pid) {
            // Claude exited without SessionEnd (crash, closed window).
            let _ = std::fs::remove_file(entry.path());
            continue;
        }
        let Some(state) = f.get("state").and_then(|s| State::parse(s)) else { continue };
        let cwd = f.get("cwd").map(String::as_str).unwrap_or_default();
        sessions.push(Session {
            id: id.to_string(),
            project: cwd.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or_default().to_string(),
            state,
            updated_ms: f.get("updated").and_then(|u| u.parse().ok()).unwrap_or(0),
            pid,
        });
    }
    sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_ms));
    let limits = std::fs::read_to_string(dir.join("limits.txt")).map(|t| parse_limits(&t)).unwrap_or_default();
    (sessions, limits)
}

fn alive(pid: u32) -> bool {
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut code = 0;
        let running = GetExitCodeProcess(process, &mut code).is_ok() && code == STILL_ACTIVE.0 as u32;
        let _ = CloseHandle(process);
        running
    }
}

fn parse_limits(text: &str) -> Limits {
    let f = fields(text);
    let window = |key: &str| {
        let (used, resets) = f.get(key)?.split_once(' ')?;
        Some(Window { used_percentage: used.parse().ok()?, resets_at: resets.parse().ok()? })
    };
    Limits {
        five_hour: window("five_hour"),
        seven_day: window("seven_day"),
        updated: f.get("updated").and_then(|u| u.parse().ok()).unwrap_or(0),
    }
}

fn fields(text: &str) -> HashMap<String, String> {
    text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn string(json: &JsonObject, key: &str) -> Option<String> {
    json.GetNamedString(&HSTRING::from(key)).ok().map(|s| s.to_string())
}

fn object(json: &JsonObject, key: &str) -> Option<JsonObject> {
    json.GetNamedObject(&HSTRING::from(key)).ok()
}

fn window(rate_limits: &JsonObject, key: &str) -> Option<Window> {
    let w = object(rate_limits, key)?;
    Some(Window {
        used_percentage: w.GetNamedNumber(h!("used_percentage")).ok()?,
        resets_at: w.GetNamedNumber(h!("resets_at")).ok()? as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_events_map_to_states() {
        let to = |event, kind| match change(event, kind) {
            Some(Change::To(s)) => Some(s),
            _ => None,
        };
        assert_eq!(to("SessionStart", None), Some(State::Open));
        assert_eq!(to("UserPromptSubmit", None), Some(State::Working));
        assert_eq!(to("PostToolUse", None), Some(State::Working));
        assert_eq!(to("PermissionRequest", None), Some(State::Permission));
        assert_eq!(to("Notification", Some("permission_prompt")), Some(State::Permission));
        assert_eq!(to("Notification", Some("idle_prompt")), None);
        assert_eq!(to("Stop", None), Some(State::Done));
        assert!(matches!(change("SessionEnd", None), Some(Change::End)));
        assert!(change("PreCompact", None).is_none());
    }

    #[test]
    fn summary_puts_the_user_first() {
        let session = |id: &str, project: &str, state| Session { id: id.into(), project: project.into(), state, updated_ms: 0, pid: 0 };
        let all = |_: &Session| true;
        let text = |line: (String, String, u32)| format!("{} / {}", line.0, line.1);
        assert_eq!(text(summary(&[], all)), "Claude / Açık oturum yok");
        assert_eq!(text(summary(&[session("a", "web", State::Open)], all)), "Claude / web");
        let open = [session("a", "web", State::Open), session("b", "api", State::Open)];
        assert_eq!(text(summary(&open, all)), "Claude / 2 oturum açık");
        let busy = [session("a", "web", State::Working), session("b", "api", State::Working)];
        assert_eq!(text(summary(&busy, all)), "Çalışıyor / 2 oturum");
        let mixed = [session("a", "web", State::Working), session("b", "api", State::Permission), session("c", "app", State::Done)];
        assert_eq!(summary(&mixed, all), ("İzin bekliyor".to_string(), "api +2".to_string(), State::Permission.color()));
        let done = [session("a", "web", State::Working), session("c", "app", State::Done)];
        assert_eq!(text(summary(&done, all)), "Yanıt hazır / app +1");
        // An answer the user already saw no longer takes the line.
        assert_eq!(text(summary(&done, |_| false)), "Çalışıyor / web +1");
        crate::text::set(crate::text::Language::English);
        assert_eq!(text(summary(&open, all)), "Claude / 2 sessions open");
        assert_eq!(text(summary(&done, all)), "Answer ready / app +1");

        let limits = Limits { five_hour: Some(Window { used_percentage: 92.6, resets_at: 100 }), ..Limits::default() };
        let used = five_hour_used(&limits, 50).unwrap();
        assert!((used.0 - 92.6).abs() < 1e-3 && used.1 == 0xFFFF453A);
        assert_eq!(five_hour_used(&limits, 100).map(|l| l.0), Some(0.0)); // reset since
        assert_eq!(five_hour_used(&Limits::default(), 0), None);
        assert_eq!(usage_color(69.0), 0xFFD97757);
        assert_eq!(usage_color(70.0), 0xFFFF9F0A);
    }

    #[test]
    fn settings_get_the_hooks_once_and_lose_them_again() {
        let path = std::env::temp_dir().join(format!("island-settings-test-{}.json", std::process::id()));
        let exe = "C:/Users/me/AppData/Local/DynamicIsland/island.exe";
        // Someone's settings: another tool's Stop hook, their own status line.
        let original = "{\n  \"model\": \"opus\",\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"mine.exe\"\n  },\n  \"hooks\": {\n    \"Stop\": [\n      {\n        \"hooks\": [\n          {\n            \"type\": \"command\",\n            \"command\": \"notify.exe\"\n          }\n        ]\n      }\n    ]\n  }\n}\n";
        std::fs::write(&path, original).unwrap();
        assert_eq!(edit_settings(&path, exe, true).unwrap(), "hooks added");
        let added = std::fs::read_to_string(&path).unwrap();
        let root = json::parse(&added).unwrap();
        assert_eq!(root.get("model").and_then(json::Value::raw), Some("\"opus\""));
        // Their status line stays; the Stop event keeps their hook first, then ours.
        assert!(added.contains("\"command\": \"mine.exe\"") && !added.contains("--statusline"));
        let Some(json::Value::Array(stop)) = root.get("hooks").and_then(|h| h.get("Stop")) else { panic!("no Stop") };
        assert_eq!(stop.len(), 2);
        let Some(json::Value::Object(events)) = root.get("hooks") else { panic!("no hooks") };
        assert_eq!(events.len(), 7);
        // Again: nothing changes.
        assert_eq!(edit_settings(&path, exe, true).unwrap(), "already as wanted");
        // Removed: exactly the file they had.
        assert_eq!(edit_settings(&path, exe, false).unwrap(), "hooks removed");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        // Without a status line of their own, the island's comes and goes too.
        std::fs::write(&path, "{}").unwrap();
        edit_settings(&path, exe, true).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("island.exe --statusline"));
        edit_settings(&path, exe, false).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("json.bak-island"));
    }

    #[test]
    fn statusline_reads_rate_limits() {
        let input = r#"{"model":{"display_name":"Opus 5.5"},"rate_limits":{"five_hour":{"used_percentage":42.4,"resets_at":1760370000},"seven_day":{"used_percentage":18,"resets_at":1760900000}}}"#;
        let json = JsonObject::Parse(&HSTRING::from(input)).unwrap();
        let limits = Limits {
            five_hour: window(&object(&json, "rate_limits").unwrap(), "five_hour"),
            seven_day: window(&object(&json, "rate_limits").unwrap(), "seven_day"),
            updated: 0,
        };
        assert_eq!(limits.five_hour, Some(Window { used_percentage: 42.4, resets_at: 1760370000 }));
        assert_eq!(status_text("Opus 5.5", Some(&limits)), "Opus 5.5 · 5 sa %42 kullanıldı · hafta %18 kullanıldı");
        assert_eq!(status_text("Opus 5.5", None), "Opus 5.5");
        // The file format round-trips.
        let line = |w: Window| format!("{} {}", w.used_percentage, w.resets_at);
        let text = format!("five_hour={}\nseven_day={}\nupdated=5\n", line(limits.five_hour.unwrap()), line(limits.seven_day.unwrap()));
        assert_eq!(parse_limits(&text), Limits { updated: 5, ..limits });
    }
}
