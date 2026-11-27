//! Herramientas de kuaky para los agentes de sus chats (ver `kuaky-mcp`):
//! abrir otra sesión como sub-línea de su objetivo, ver cómo va y mandarle
//! mensajes. Así le dices a Claude "que Antigravity haga los tests y tú la
//! barra lateral" y él mismo abre esa sesión.
//!
//! Solo chats (ACP) por ahora. Las terminales quedan anotadas en
//! `docs/plan-delegar-sesiones.md`.

use super::*;
use crate::lines::{StationSource, StationState, short};
use kuaky_mcp::{AGENTS, Call, Mailbox, Reply};
use serde_json::Value;
use std::sync::OnceLock;

/// Texto de la última respuesta que se devuelve como mucho (lo más reciente).
const MAX_ANSWER: usize = 12_000;

/// Buzón de esta app (`<config>/kuaky/ipc/<pid>`), donde `kuaky mcp` deja las llamadas.
pub(crate) fn mailbox() -> Option<&'static Mailbox> {
    static MAILBOX: OnceLock<Option<Mailbox>> = OnceLock::new();
    MAILBOX
        .get_or_init(|| {
            let base = dirs::config_dir()?.join("kuaky").join("ipc");
            remove_stale(&base);
            match Mailbox::create(base.join(std::process::id().to_string())) {
                Ok(mailbox) => Some(mailbox),
                Err(err) => {
                    tracing::warn!(%err, "no se pudo crear el buzón de herramientas");
                    None
                }
            }
        })
        .as_ref()
}

/// Buzones de ejecuciones anteriores sin uso en una semana.
fn remove_stale(base: &Path) {
    let Ok(entries) = std::fs::read_dir(base) else { return };
    let week = Duration::from_secs(7 * 24 * 3600);
    for entry in entries.filter_map(Result::ok) {
        let old = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > week);
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Servidor `kuaky mcp` para la sesión `uid` (este mismo programa).
pub(super) fn mcp_server(uid: &str) -> Option<kuaky_acp::McpServer> {
    let mailbox = mailbox()?;
    Some(kuaky_acp::McpServer {
        name: kuaky_mcp::SERVER_NAME.into(),
        command: std::env::current_exe().ok()?,
        args: vec!["mcp".into()],
        env: mailbox.env(uid),
    })
}

fn parse_agent(name: &str) -> Option<Harness> {
    let name = name.trim().to_lowercase();
    if name == "antigravity" {
        return Some(Harness::Antigravity);
    }
    Harness::AGENTS.into_iter().find(|h| h.key() == name || h.label().to_lowercase() == name)
}

fn arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// Estado de un chat en palabras, para el agente que pregunta.
fn status_of(v: &ChatView) -> String {
    if v.is_waiting_permission() {
        return "waiting for the user's permission".into();
    }
    match &v.status {
        ChatStatus::Connecting => "starting".into(),
        ChatStatus::Running => "working".into(),
        ChatStatus::Ready if v.has_turns() => "finished its turn".into(),
        ChatStatus::Ready => "idle".into(),
        ChatStatus::Error(err) => format!("failed: {}", short(err, 160)),
        ChatStatus::Exited => "closed".into(),
    }
}

/// Lo último de un texto largo (lo más reciente de la respuesta).
fn tail(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let rest: String = text.chars().skip(count - max).collect();
    format!("…(earlier text cut)\n{rest}")
}

impl ProjectView {
    pub(crate) fn has_chat(&self, uid: &str, cx: &App) -> bool {
        self.chat_view_of(uid, cx).is_some()
    }

    /// Atiende una herramienta que llamó el agente del chat `call.session`.
    /// `None`: la respuesta llega después, directo al buzón (`draw`, `preview`).
    pub(crate) fn tool_call(&mut self, call: &Call, window: &mut Window, cx: &mut Context<Self>) -> Option<Reply> {
        let result = match call.tool.as_str() {
            "draw" => return self.tool_draw(call, window, cx),
            "preview" => return self.tool_preview(call, window, cx),
            "artifacts" => self.tool_artifacts(call),
            "open_session" => self.tool_open_session(&call.session, &call.args, window, cx),
            "list_sessions" => Ok(self.tool_list_sessions(&call.session, cx)),
            "read_session" => self.tool_read_session(&call.session, &call.args, cx),
            "send_to_session" => self.tool_send(&call.session, &call.args, cx),
            other => Err(format!("Unknown tool: {other}")),
        };
        cx.notify();
        Some(match result {
            Ok(text) => Reply::ok(text),
            Err(text) => Reply::error(text),
        })
    }

    /// Abre otro agente como chat, como sub-línea de la línea de quien llama
    /// (o en la misma, si es relevo), y le manda la tarea sin preguntar.
    fn tool_open_session(&mut self, caller: &str, args: &Value, window: &mut Window, cx: &mut Context<Self>) -> Result<String, String> {
        let agent = arg(args, "agent").ok_or("Missing `agent`.")?;
        let harness = parse_agent(agent).ok_or_else(|| format!("Unknown agent `{agent}`. Use one of: {}.", AGENTS.join(", ")))?;
        let task = arg(args, "task").ok_or("Missing `task`: write the brief for the other agent.")?;
        let handoff = arg(args, "mode") == Some("handoff");
        let worktree = args.get("worktree").and_then(Value::as_bool).unwrap_or(false);
        if !self.chat_agents.contains_key(&harness) && harness.chat_command().is_none() {
            return Err(format!(
                "{} can't be opened as a chat (ACP) on this machine. Pick another agent, or tell the user to install its ACP mode.",
                harness.label()
            ));
        }
        let caller_view = self.chat_view_of(caller, cx).ok_or("Your session is not open in kuaky.")?;
        let (caller_label, caller_cwd) = {
            let v = caller_view.read(cx);
            (v.launch.label, v.launch.cwd.clone())
        };
        let line = self.lines.line_of(caller).cloned().ok_or("Your session has no line in kuaky.")?;

        // Tope (Settings → Agents): sesiones abiertas desde la principal que siguen abiertas.
        let root = self.lines.root_of(caller);
        let open: Vec<String> =
            self.lines.opened_from(&root).iter().map(|(_, s)| s.uid.clone()).filter(|uid| self.chat_view_of(uid, cx).is_some()).collect();
        let limit = state::get(cx).settings.session_limit();
        if let Some(max) = limit.filter(|max| open.len() >= *max) {
            return Err(format!(
                "There are already {max} sessions open besides the main one ({}), the limit set in kuaky Settings → Agents. \
                 Ask the user to close one or raise the limit, or give the work to one of them with send_to_session.",
                open.join(", ")
            ));
        }

        let title = arg(args, "title").map(|t| short(t, 48)).unwrap_or_else(|| short(task, 48));
        let worktree = match worktree {
            true => Some(self.create_worktree_for(harness, cx).ok_or("Could not create a git worktree (is this folder a git repository?).")?),
            false => None,
        };
        let line_id = if handoff {
            line.id.clone()
        } else {
            let id = self.lines.create(&title, line.kind, Some(line.id.clone()), false);
            // El nombre viene del agente que la abrió: el título del otro no lo cambia.
            if let Some(sub) = self.lines.get_mut(&id) {
                sub.titled = true;
            }
            id
        };
        let cwd = match &worktree {
            Some((path, branch)) => {
                if !handoff {
                    self.lines.set_branch(&line_id, Some(branch.clone()));
                }
                path.clone()
            }
            None => caller_cwd.clone(),
        };

        let intro = if handoff {
            format!("{caller_label} handed this goal over to you from kuaky and stopped working on it.")
        } else {
            format!("{caller_label} opened this session from kuaky to work on part of that goal while it keeps doing the rest.")
        };
        let place = match &worktree {
            Some((path, branch)) => format!(
                " You work in your own git worktree ({}, branch {branch}). It starts from the last commit: uncommitted changes in {} are not there.",
                path.display(),
                caller_cwd.display()
            ),
            None if handoff => String::new(),
            None => format!(" You work in the same folder as {caller_label} at the same time: only edit the files of your task."),
        };
        // La primera línea nombra la sesión y su estación en el mapa.
        let prompt = format!("{title}\n\n{task}\n\n{intro}{place}\n{}", line.relay_note());

        let opts = ChatOpts {
            line: Some(line_id.clone()),
            cwd: Some(cwd.clone()),
            role: handoff.then(|| "relay".to_string()),
            opened_by: Some(caller.to_string()),
            background: true,
            ..Default::default()
        };
        let before = self.tabs.len();
        self.open_chat_with(harness, opts, window, cx);
        let child = self
            .tabs
            .get(before)
            .and_then(|t| match &t.kind {
                TabKind::Chat { view, .. } => Some(view.clone()),
                _ => None,
            })
            .ok_or("kuaky could not open the chat.")?;
        let uid = child.read(cx).uid.clone();
        child.update(cx, |v, cx| v.submit_text(prompt, cx));
        if handoff {
            let station = format!("Hand-off · {}", harness.label());
            self.lines.add_station(&line.id, &station, StationSource::Connection, Some(uid.clone()), StationState::Active);
        }
        self.lines.save();

        let line_name = self.lines.get(&line_id).map(|l| l.name.clone()).unwrap_or_default();
        let where_ = if handoff { format!("on your line \"{line_name}\"") } else { format!("on the sub-line \"{line_name}\"") };
        Ok(format!(
            "Opened {} as session `{uid}` {where_}, in {}. It is working on the task now{}. \
             Use read_session with this id when the user asks how it is going. Sessions open besides the main one: {}.",
            harness.label(),
            cwd.display(),
            if handoff { "" } else { "; keep doing your part" },
            match limit {
                Some(max) => format!("{} of {max}", open.len() + 1),
                None => format!("{} (no limit)", open.len() + 1),
            },
        ))
    }

    fn tool_list_sessions(&self, caller: &str, cx: &App) -> String {
        let root = self.lines.root_of(caller);
        let mut rows = Vec::new();
        let mut row = |uid: &str, harness: &str, line: &str, extra: &str| {
            let agent = Harness::from_key(harness).map(|h| h.label()).unwrap_or(harness);
            let status = self.chat_view_of(uid, cx).map(|v| status_of(v.read(cx))).unwrap_or_else(|| "closed".into());
            let you = if uid == caller { " (you)" } else { "" };
            rows.push(format!("- `{uid}`{you} · {agent} · line \"{line}\" · {status}{extra}"));
        };
        if let (Some(line), Some(s)) = (self.lines.line_of(&root), self.lines.session(&root)) {
            row(&root, &s.harness, &line.name, " · main session");
        }
        for (line, s) in self.lines.opened_from(&root) {
            let by = s.opened_by.as_deref().filter(|by| *by != root).map(|by| format!(" · opened by `{by}`")).unwrap_or_default();
            row(&s.uid, &s.harness, &line.name, &by);
        }
        if rows.len() <= 1 {
            rows.push("No other sessions were opened from this conversation yet.".into());
        }
        rows.join("\n")
    }

    /// Solo se ven las sesiones de la misma familia (la principal y las que salieron de ella).
    fn family_member(&self, caller: &str, target: &str) -> Result<(), String> {
        if self.lines.root_of(caller) == self.lines.root_of(target) {
            Ok(())
        } else {
            Err(format!("`{target}` is not one of the sessions opened from this conversation. See list_sessions."))
        }
    }

    fn tool_read_session(&self, caller: &str, args: &Value, cx: &App) -> Result<String, String> {
        let target = arg(args, "session").ok_or("Missing `session`.")?;
        self.family_member(caller, target)?;
        let view = self.chat_view_of(target, cx).ok_or_else(|| format!("`{target}` is closed."))?;
        let v = view.read(cx);
        let status = status_of(v);
        let mut out = format!("{} · {status}", v.launch.label);
        if let Some(title) = &v.title {
            out.push_str(&format!("\nTitle: {title}"));
        }
        let plan = v.plan_entries();
        if !plan.is_empty() {
            out.push_str("\nPlan:");
            for e in plan {
                let mark = match e.status.as_str() {
                    "completed" => "x",
                    "in_progress" => "~",
                    _ => " ",
                };
                out.push_str(&format!("\n- [{mark}] {}", e.content));
            }
        }
        if v.queued() > 0 {
            out.push_str(&format!("\n{} message(s) queued for it.", v.queued()));
        }
        match v.last_answer() {
            Some(answer) if status == "working" => out.push_str(&format!("\n\nLatest text so far:\n{}", tail(&answer, MAX_ANSWER))),
            Some(answer) => out.push_str(&format!("\n\nLatest answer:\n{}", tail(&answer, MAX_ANSWER))),
            None => out.push_str("\n\nNo answer yet."),
        }
        Ok(out)
    }

    fn tool_send(&mut self, caller: &str, args: &Value, cx: &mut Context<Self>) -> Result<String, String> {
        let target = arg(args, "session").ok_or("Missing `session`.")?;
        let message = arg(args, "message").ok_or("Missing `message`.")?;
        if target == caller {
            return Err("That is your own session.".into());
        }
        self.family_member(caller, target)?;
        let view = self.chat_view_of(target, cx).ok_or_else(|| format!("`{target}` is closed."))?;
        let from = self.chat_view_of(caller, cx).map(|v| v.read(cx).launch.label).unwrap_or("The main session");
        let busy = match &view.read(cx).status {
            ChatStatus::Error(_) | ChatStatus::Exited => return Err(format!("`{target}` is {}.", status_of(view.read(cx)))),
            ChatStatus::Running | ChatStatus::Connecting => true,
            ChatStatus::Ready => false,
        };
        let text = format!("Message from {from} (the session that opened you):\n\n{message}");
        view.update(cx, |v, cx| v.submit_text(text, cx));
        Ok(if busy { "Queued: it is busy and gets it when its turn ends.".into() } else { "Sent.".into() })
    }
}

#[cfg(test)]
mod tests {
    // Sin `super::*`: gpui_kit trae su propio `#[test]`.
    use super::{AGENTS, parse_agent, tail};
    use crate::project::Harness;

    #[test]
    fn agent_names() {
        assert_eq!(parse_agent("Antigravity"), Some(Harness::Antigravity));
        assert_eq!(parse_agent("agy"), Some(Harness::Antigravity));
        assert_eq!(parse_agent("claude"), Some(Harness::Claude));
        assert_eq!(parse_agent("Claude Code"), Some(Harness::Claude));
        assert_eq!(parse_agent("gemini cli"), Some(Harness::Gemini));
        assert_eq!(parse_agent("shell"), None);
        for name in AGENTS {
            assert!(parse_agent(name).is_some(), "{name}");
        }
    }

    #[test]
    fn tail_keeps_the_end() {
        assert_eq!(tail("hola", 10), "hola");
        assert_eq!(tail("ñandú-fin", 3), "…(earlier text cut)\nfin");
    }
}
