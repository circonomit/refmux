use crate::Config;
use crate::project;
use crate::rpc;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io::{self, BufReader, BufWriter, Stdout};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitCode, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};
use url::Url;

const INITIALIZE_ID: &str = "refmux/initialize";
const SHUTDOWN_ID: &str = "refmux/shutdown";
const TICK: Duration = Duration::from_secs(5);
const STOP_GRACE: Duration = Duration::from_secs(5);
const EXIT_WAIT: Duration = Duration::from_secs(2);

const REQUEST_FAILED: i64 = -32803;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_REQUEST: i64 = -32600;

const ERROR: u8 = 1;
const WARNING: u8 = 2;
const INFO: u8 = 3;

/// What `dbt lsp` 2.0.8 answers to `initialize`. The client needs capabilities before any project
/// server exists; the first server's own answer is compared against this and drift is logged.
fn dbt_lsp_capabilities() -> Value {
    json!({
        "codeActionProvider": {"codeActionKinds": ["source.fixAll", "source.fixAll.dbtLintFix"]},
        "codeLensProvider": {"resolveProvider": false},
        "completionProvider": {
            "completionItem": {"labelDetailsSupport": true},
            "resolveProvider": false,
            "triggerCharacters": ["{", "'", "."]
        },
        "definitionProvider": true,
        "documentFormattingProvider": true,
        "documentSymbolProvider": false,
        "executeCommandProvider": {"commands": [
            "dbt.listNodes", "dbt.getCurrentNode", "dbt.compileFile", "dbt.compileLsp",
            "dbt.clearTarget", "dbt.getProjectInfo", "dbt.show"
        ]},
        "hoverProvider": true,
        "inlayHintProvider": true,
        "referencesProvider": true,
        "renameProvider": true,
        "semanticTokensProvider": {
            "full": true,
            "legend": {
                "tokenModifiers": [],
                "tokenTypes": ["property", "function", "macro", "type", "variable", "keyword"]
            },
            "range": true
        },
        "signatureHelpProvider": {"retriggerCharacters": [","], "triggerCharacters": ["("]},
        "textDocumentSync": {"change": 2, "openClose": true, "save": {}},
        "workspace": {"fileOperations": {"willRename": {"filters": [
            {"pattern": {"glob": "**/*.{sql,csv}", "matches": "file"}, "scheme": "file"}
        ]}}}
    })
}

enum Event {
    Client(Value),
    ClientClosed,
    Server(u32, Value),
    ServerClosed(u32),
    Tick,
}

struct Project {
    dir: PathBuf,
    child: Child,
    to_child: Sender<Value>,
    ready: bool,
    queue: Vec<Value>,
    open_documents: HashSet<String>,
    idle_since: Option<Instant>,
    diagnosed: HashSet<String>,
    /// Client requests forwarded to this server, keyed like `Mux::client_requests`.
    pending: HashMap<String, Value>,
    /// Namespaced registration id -> method, unregistered at the client when the server stops.
    registrations: HashMap<String, String>,
}

struct Mux {
    config: Config,
    events: Sender<Event>,
    out: BufWriter<Stdout>,
    initialize_params: Value,
    next_project_id: u32,
    projects: HashMap<u32, Project>,
    project_by_dir: HashMap<PathBuf, u32>,
    documents: HashMap<String, u32>,
    client_requests: HashMap<String, u32>,
    /// Server-to-client requests get mux-wide ids, since every server numbers its own from 1.
    server_requests: HashMap<i64, (u32, Value)>,
    next_server_request_id: i64,
    /// Progress tokens created by a server, namespaced per project so two servers cannot collide.
    progress_tokens: HashMap<String, (u32, Value)>,
    stopping: Vec<(Child, Instant)>,
    failed_dirs: HashSet<PathBuf>,
    last_active: Option<u32>,
    shutdown_requested: bool,
    capabilities_checked: bool,
}

pub fn run(config: Config) -> ExitCode {
    let (events, inbox) = mpsc::channel();
    spawn_client_reader(events.clone());
    spawn_ticker(events.clone());
    let mut mux = Mux::new(config, events);
    for event in inbox {
        match event {
            Event::Client(message) => {
                if let Some(code) = mux.on_client(message) {
                    return code;
                }
            }
            Event::ClientClosed => {
                mux.stop_all();
                return mux.exit_code();
            }
            Event::Server(id, message) => mux.on_server(id, message),
            Event::ServerClosed(id) => mux.on_server_closed(id),
            Event::Tick => mux.on_tick(),
        }
    }
    ExitCode::SUCCESS
}

fn spawn_client_reader(events: Sender<Event>) {
    thread::spawn(move || {
        let mut reader = BufReader::new(io::stdin().lock());
        loop {
            match rpc::read_message(&mut reader) {
                Ok(Some(message)) => {
                    if events.send(Event::Client(message)).is_err() {
                        return;
                    }
                }
                Ok(None) => break,
                Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                    eprintln!("refmux: skipping malformed client message: {error}");
                }
                Err(_) => break,
            }
        }
        let _ = events.send(Event::ClientClosed);
    });
}

fn spawn_ticker(events: Sender<Event>) {
    thread::spawn(move || {
        loop {
            thread::sleep(TICK);
            if events.send(Event::Tick).is_err() {
                return;
            }
        }
    });
}

fn spawn_server_reader(id: u32, stdout: ChildStdout, events: Sender<Event>) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            match rpc::read_message(&mut reader) {
                Ok(Some(message)) => {
                    if events.send(Event::Server(id, message)).is_err() {
                        return;
                    }
                }
                Ok(None) => break,
                Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                    eprintln!("refmux: skipping malformed server message: {error}");
                }
                Err(_) => break,
            }
        }
        let _ = events.send(Event::ServerClosed(id));
    });
}

/// Dropping the project's sender ends this thread, which closes the server's stdin.
fn spawn_server_writer(stdin: ChildStdin, inbox: Receiver<Value>) {
    thread::spawn(move || {
        let mut writer = BufWriter::new(stdin);
        for message in inbox {
            if rpc::write_message(&mut writer, &message).is_err() {
                return;
            }
        }
    });
}

fn id_key(id: &Value) -> String {
    id.to_string()
}

fn uri_to_path(uri: &str) -> Option<PathBuf> {
    Url::parse(uri).ok()?.to_file_path().ok()
}

fn first_file_uri(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if text.starts_with("file://") => Some(text.clone()),
        Value::Array(items) => items.iter().find_map(first_file_uri),
        Value::Object(map) => map.values().find_map(first_file_uri),
        _ => None,
    }
}

fn token_text(token: &Value) -> String {
    token
        .as_str()
        .map_or_else(|| token.to_string(), str::to_owned)
}

fn is_file_operation(method: &str) -> bool {
    matches!(
        method,
        "workspace/willRenameFiles"
            | "workspace/didRenameFiles"
            | "workspace/willCreateFiles"
            | "workspace/didCreateFiles"
            | "workspace/willDeleteFiles"
            | "workspace/didDeleteFiles"
    )
}

impl Mux {
    fn new(config: Config, events: Sender<Event>) -> Self {
        Self {
            config,
            events,
            out: BufWriter::new(io::stdout()),
            initialize_params: Value::Null,
            next_project_id: 1,
            projects: HashMap::new(),
            project_by_dir: HashMap::new(),
            documents: HashMap::new(),
            client_requests: HashMap::new(),
            server_requests: HashMap::new(),
            next_server_request_id: 1,
            progress_tokens: HashMap::new(),
            stopping: Vec::new(),
            failed_dirs: HashSet::new(),
            last_active: None,
            shutdown_requested: false,
            capabilities_checked: false,
        }
    }

    fn exit_code(&self) -> ExitCode {
        if self.shutdown_requested {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    }

    // ---- client -> mux ----

    fn on_client(&mut self, message: Value) -> Option<ExitCode> {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let id = message.get("id").cloned();
        match (method.as_deref(), id) {
            (None, Some(id)) => self.on_client_response(id, message),
            (Some("initialize"), Some(id)) => self.on_initialize(id, &message),
            (Some("initialized"), None) => {}
            (Some("shutdown"), Some(id)) => {
                self.shutdown_requested = true;
                self.stop_all();
                self.reply(id, Value::Null);
            }
            (Some("exit"), None) => {
                self.stop_all();
                return Some(self.exit_code());
            }
            (Some(_), Some(id)) if self.shutdown_requested => {
                self.reply_error(id, INVALID_REQUEST, "the server is shutting down");
            }
            (Some(_), None) if self.shutdown_requested => {}
            (Some("$/cancelRequest"), None) => self.on_cancel(message),
            (Some("window/workDoneProgress/cancel"), None) => self.on_progress_cancel(message),
            (Some("workspace/didChangeConfiguration"), None) => self.broadcast(&message),
            (Some("workspace/didChangeWatchedFiles"), None) => self.on_watched_files(&message),
            (Some("workspace/executeCommand"), Some(id)) => self.on_execute_command(id, message),
            (Some(method), id) if is_file_operation(method) => self.on_file_operation(id, message),
            (Some(method), id) if method.starts_with("textDocument/") => {
                let method = method.to_owned();
                self.on_document(&method, id, message);
            }
            (Some(_), Some(id)) => {
                self.reply_error(id, METHOD_NOT_FOUND, "not supported by refmux");
            }
            _ => {}
        }
        None
    }

    fn on_initialize(&mut self, id: Value, message: &Value) {
        self.initialize_params = message.get("params").cloned().unwrap_or(Value::Null);
        let mut capabilities = dbt_lsp_capabilities();
        if !self.config.formatting
            && let Some(map) = capabilities.as_object_mut()
        {
            map.remove("documentFormattingProvider");
        }
        self.reply(
            id,
            json!({
                "capabilities": capabilities,
                "serverInfo": {"name": "refmux", "version": env!("CARGO_PKG_VERSION")}
            }),
        );
    }

    fn on_client_response(&mut self, id: Value, mut message: Value) {
        let Some((project, original)) = id
            .as_i64()
            .and_then(|mux_id| self.server_requests.remove(&mux_id))
        else {
            return;
        };
        message["id"] = original;
        self.forward(project, message);
    }

    fn on_document(&mut self, method: &str, id: Option<Value>, message: Value) {
        let uri = message
            .pointer("/params/textDocument/uri")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let target = uri.as_deref().and_then(|uri| {
            if method == "textDocument/didOpen" {
                self.open_document(uri)
            } else {
                self.documents
                    .get(uri)
                    .copied()
                    .or_else(|| self.running_project_for(uri))
            }
        });
        let Some(target) = target else {
            if let Some(id) = id {
                self.reply(id, Value::Null);
            }
            return;
        };
        if method == "textDocument/didClose"
            && let Some(uri) = &uri
        {
            self.documents.remove(uri);
            if let Some(project) = self.projects.get_mut(&target) {
                project.open_documents.remove(uri);
                if project.open_documents.is_empty() {
                    project.idle_since = Some(Instant::now());
                }
            }
        }
        self.last_active = Some(target);
        self.forward_request_or_notification(target, id, message);
    }

    fn open_document(&mut self, uri: &str) -> Option<u32> {
        let dir = project::find_project(&uri_to_path(uri)?)?;
        let id = match self.project_by_dir.get(&dir) {
            Some(&id) => id,
            None => self.start_project(&dir)?,
        };
        let project = self.projects.get_mut(&id)?;
        project.open_documents.insert(uri.to_owned());
        project.idle_since = None;
        self.documents.insert(uri.to_owned(), id);
        Some(id)
    }

    fn running_project_for(&self, uri: &str) -> Option<u32> {
        let dir = project::find_project(&uri_to_path(uri)?)?;
        self.project_by_dir.get(&dir).copied()
    }

    fn on_cancel(&mut self, message: Value) {
        let target = message
            .pointer("/params/id")
            .and_then(|id| self.client_requests.get(&id_key(id)).copied());
        if let Some(target) = target {
            self.forward(target, message);
        }
    }

    fn on_progress_cancel(&mut self, mut message: Value) {
        let Some((project, original)) = message
            .pointer("/params/token")
            .and_then(|token| self.progress_tokens.get(&token_text(token)).cloned())
        else {
            return;
        };
        message["params"]["token"] = original;
        self.forward(project, message);
    }

    fn broadcast(&mut self, message: &Value) {
        let ids: Vec<u32> = self.projects.keys().copied().collect();
        for id in ids {
            self.forward(id, message.clone());
        }
    }

    fn on_watched_files(&mut self, message: &Value) {
        let Some(changes) = message.pointer("/params/changes").and_then(Value::as_array) else {
            return;
        };
        let mut per_project: HashMap<u32, Vec<Value>> = HashMap::new();
        for change in changes {
            let target = change
                .get("uri")
                .and_then(Value::as_str)
                .and_then(|uri| self.running_project_for(uri));
            if let Some(target) = target {
                per_project.entry(target).or_default().push(change.clone());
            }
        }
        for (target, changes) in per_project {
            self.forward(
                target,
                json!({
                    "jsonrpc": "2.0",
                    "method": "workspace/didChangeWatchedFiles",
                    "params": {"changes": changes}
                }),
            );
        }
    }

    fn on_execute_command(&mut self, id: Value, message: Value) {
        let target = message
            .pointer("/params/arguments")
            .and_then(first_file_uri)
            .and_then(|uri| {
                self.documents
                    .get(&uri)
                    .copied()
                    .or_else(|| self.running_project_for(&uri))
            })
            .or(self.last_active.filter(|id| self.projects.contains_key(id)));
        let Some(target) = target else {
            self.reply_error(id, REQUEST_FAILED, "no dbt project server is running");
            return;
        };
        self.forward_request_or_notification(target, Some(id), message);
    }

    fn on_file_operation(&mut self, id: Option<Value>, message: Value) {
        let target = message
            .pointer("/params/files/0/oldUri")
            .or_else(|| message.pointer("/params/files/0/uri"))
            .and_then(Value::as_str)
            .and_then(|uri| self.running_project_for(uri));
        match (target, id) {
            (Some(target), id) => self.forward_request_or_notification(target, id, message),
            (None, Some(id)) => self.reply(id, Value::Null),
            (None, None) => {}
        }
    }

    fn forward_request_or_notification(&mut self, target: u32, id: Option<Value>, message: Value) {
        if let Some(id) = id {
            let key = id_key(&id);
            self.client_requests.insert(key.clone(), target);
            if let Some(project) = self.projects.get_mut(&target) {
                project.pending.insert(key, id);
            }
        }
        self.forward(target, message);
    }

    fn forward(&mut self, target: u32, message: Value) {
        let Some(project) = self.projects.get_mut(&target) else {
            return;
        };
        if project.ready {
            let _ = project.to_child.send(message);
        } else {
            project.queue.push(message);
        }
    }

    // ---- project servers ----

    fn start_project(&mut self, dir: &Path) -> Option<u32> {
        let (program, base_args) = self.config.server_command.split_first()?;
        let profiles = project::profiles(dir);
        let mut command = Command::new(program);
        command
            .args(base_args)
            .arg("--project-dir")
            .arg(dir)
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(profiles_dir) = &profiles.flag {
            command.arg("--profiles-dir").arg(profiles_dir);
        }
        let placeholders = if self.config.env_placeholders {
            project::env_placeholders(dir, profiles.file.as_deref())
        } else {
            Vec::new()
        };
        command.envs(placeholders.iter().map(|(name, value)| (name, value)));

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                if self.failed_dirs.insert(dir.to_path_buf()) {
                    let command_line = self.config.server_command.join(" ");
                    self.show(
                        ERROR,
                        format!(
                            "could not start `{command_line}` for {}: {error}",
                            dir.display()
                        ),
                    );
                }
                return None;
            }
        };
        self.failed_dirs.remove(dir);
        let id = self.next_project_id;
        self.next_project_id += 1;
        let (stdin, stdout) = (child.stdin.take()?, child.stdout.take()?);
        let (to_child, inbox) = mpsc::channel();
        spawn_server_writer(stdin, inbox);
        spawn_server_reader(id, stdout, self.events.clone());

        if !placeholders.is_empty() {
            let names: Vec<&str> = placeholders.iter().map(|(name, _)| name.as_str()).collect();
            self.log(
                INFO,
                format!(
                    "{}: placeholder values for unset {}",
                    dir.display(),
                    names.join(", ")
                ),
            );
        }

        let uri = Url::from_file_path(dir)
            .map(String::from)
            .unwrap_or_default();
        let name = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut params = self.initialize_params.clone();
        params["processId"] = json!(std::process::id());
        params["rootUri"] = json!(uri);
        params["rootPath"] = json!(dir.display().to_string());
        params["workspaceFolders"] = json!([{"uri": uri, "name": name}]);
        let _ = to_child.send(json!({
            "jsonrpc": "2.0",
            "id": INITIALIZE_ID,
            "method": "initialize",
            "params": params
        }));

        self.projects.insert(
            id,
            Project {
                dir: dir.to_path_buf(),
                child,
                to_child,
                ready: false,
                queue: Vec::new(),
                open_documents: HashSet::new(),
                idle_since: None,
                diagnosed: HashSet::new(),
                pending: HashMap::new(),
                registrations: HashMap::new(),
            },
        );
        self.project_by_dir.insert(dir.to_path_buf(), id);
        Some(id)
    }

    // ---- server -> mux ----

    fn on_server(&mut self, id: u32, mut message: Value) {
        if !self.projects.contains_key(&id) {
            return;
        }
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        match (method, message.get("id").cloned()) {
            (None, Some(response_id)) => self.on_server_response(id, &response_id, message),
            (Some(method), Some(request_id)) => {
                match method.as_str() {
                    "window/workDoneProgress/create" => {
                        self.namespace_created_token(id, &mut message);
                    }
                    "client/registerCapability" => self.namespace_registrations(id, &mut message),
                    "client/unregisterCapability" => {
                        self.namespace_unregistrations(id, &mut message);
                    }
                    _ => {}
                }
                let mux_id = self.next_server_request_id;
                self.next_server_request_id += 1;
                self.server_requests.insert(mux_id, (id, request_id));
                message["id"] = json!(mux_id);
                self.send(&message);
            }
            (Some(method), None) => {
                match method.as_str() {
                    "$/progress" => self.namespace_progress(id, &mut message),
                    "textDocument/publishDiagnostics" => self.track_diagnostics(id, &message),
                    _ => {}
                }
                self.send(&message);
            }
            (None, None) => {}
        }
    }

    fn on_server_response(&mut self, id: u32, response_id: &Value, message: Value) {
        if *response_id == INITIALIZE_ID {
            self.on_project_ready(id, &message);
            return;
        }
        if *response_id == SHUTDOWN_ID {
            return;
        }
        let key = id_key(response_id);
        self.client_requests.remove(&key);
        if let Some(project) = self.projects.get_mut(&id) {
            project.pending.remove(&key);
        }
        self.send(&message);
    }

    fn on_project_ready(&mut self, id: u32, message: &Value) {
        if let Some(error) = message.get("error") {
            if let Some(project) = self.remove_project(id) {
                self.show(
                    ERROR,
                    format!(
                        "dbt lsp for {} rejected initialize: {error}",
                        project.dir.display()
                    ),
                );
                self.stop(project);
            }
            return;
        }
        let Some(project) = self.projects.get_mut(&id) else {
            return;
        };
        project.ready = true;
        let _ = project
            .to_child
            .send(json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}));
        for queued in project.queue.drain(..) {
            let _ = project.to_child.send(queued);
        }
        let dir = project.dir.display().to_string();
        let version = message
            .pointer("/result/serverInfo/version")
            .and_then(Value::as_str)
            .unwrap_or("unknown version")
            .to_owned();
        self.log(INFO, format!("started dbt lsp {version} for {dir}"));
        if !self.capabilities_checked {
            self.capabilities_checked = true;
            let actual = message
                .pointer("/result/capabilities")
                .cloned()
                .unwrap_or(Value::Null);
            if actual != dbt_lsp_capabilities() {
                self.log(
                    WARNING,
                    format!(
                        "dbt lsp {version} advertises different capabilities than the ones this mux forwards (pinned to 2.0.8): {actual}"
                    ),
                );
            }
        }
    }

    fn namespace_created_token(&mut self, id: u32, message: &mut Value) {
        let Some(token) = message.pointer("/params/token").cloned() else {
            return;
        };
        let namespaced = format!("refmux/{id}/{}", token_text(&token));
        self.progress_tokens.insert(namespaced.clone(), (id, token));
        message["params"]["token"] = json!(namespaced);
    }

    /// Only tokens the server created get rewritten; tokens the client handed in stay as they are.
    fn namespace_progress(&mut self, id: u32, message: &mut Value) {
        let Some(token) = message.pointer("/params/token") else {
            return;
        };
        let namespaced = format!("refmux/{id}/{}", token_text(token));
        if !self.progress_tokens.contains_key(&namespaced) {
            return;
        }
        if message
            .pointer("/params/value/kind")
            .and_then(Value::as_str)
            == Some("end")
        {
            self.progress_tokens.remove(&namespaced);
        }
        message["params"]["token"] = json!(namespaced);
    }

    /// Every `dbt lsp` registers its file watcher under the same id; the client sees one server.
    fn namespace_registrations(&mut self, id: u32, message: &mut Value) {
        let Some(registrations) = message
            .pointer_mut("/params/registrations")
            .and_then(Value::as_array_mut)
        else {
            return;
        };
        let Some(project) = self.projects.get_mut(&id) else {
            return;
        };
        for registration in registrations {
            let (Some(original), Some(method)) = (
                registration.get("id").and_then(Value::as_str),
                registration.get("method").and_then(Value::as_str),
            ) else {
                continue;
            };
            let namespaced = format!("refmux/{id}/{original}");
            project
                .registrations
                .insert(namespaced.clone(), method.to_owned());
            registration["id"] = json!(namespaced);
        }
    }

    /// The protocol spells the field `unregisterations`.
    fn namespace_unregistrations(&mut self, id: u32, message: &mut Value) {
        let Some(unregistrations) = message
            .pointer_mut("/params/unregisterations")
            .and_then(Value::as_array_mut)
        else {
            return;
        };
        let Some(project) = self.projects.get_mut(&id) else {
            return;
        };
        for unregistration in unregistrations {
            let Some(original) = unregistration.get("id").and_then(Value::as_str) else {
                continue;
            };
            let namespaced = format!("refmux/{id}/{original}");
            project.registrations.remove(&namespaced);
            unregistration["id"] = json!(namespaced);
        }
    }

    fn track_diagnostics(&mut self, id: u32, message: &Value) {
        let Some(uri) = message.pointer("/params/uri").and_then(Value::as_str) else {
            return;
        };
        let empty = message
            .pointer("/params/diagnostics")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty);
        if let Some(project) = self.projects.get_mut(&id) {
            if empty {
                project.diagnosed.remove(uri);
            } else {
                project.diagnosed.insert(uri.to_owned());
            }
        }
    }

    fn on_server_closed(&mut self, id: u32) {
        let Some(mut project) = self.remove_project(id) else {
            return;
        };
        let status = match project.child.try_wait() {
            Ok(Some(status)) => status.to_string(),
            _ => {
                let _ = project.child.kill();
                project
                    .child
                    .wait()
                    .map_or_else(|error| error.to_string(), |status| status.to_string())
            }
        };
        let dir = project.dir.display().to_string();
        if project.ready {
            self.show(
                WARNING,
                format!("dbt lsp for {dir} exited ({status}); open a file there to restart it"),
            );
        } else {
            let command_line = self.config.server_command.join(" ");
            self.show(
                ERROR,
                format!(
                    "dbt lsp for {dir} exited during startup ({status}); `{command_line}` must be dbt Fusion (dbt-core and dbt-oss have no `lsp` subcommand)"
                ),
            );
        }
        self.release(&project);
    }

    // ---- lifecycle ----

    fn on_tick(&mut self) {
        if let Some(timeout) = self.config.idle_timeout {
            let idle: Vec<u32> = self
                .projects
                .iter()
                .filter(|(_, project)| {
                    project.ready
                        && project.open_documents.is_empty()
                        && project.pending.is_empty()
                        && project
                            .idle_since
                            .is_some_and(|since| since.elapsed() >= timeout)
                })
                .map(|(id, _)| *id)
                .collect();
            for id in idle {
                if let Some(project) = self.remove_project(id) {
                    self.log(
                        INFO,
                        format!("stopping idle dbt lsp for {}", project.dir.display()),
                    );
                    self.stop(project);
                }
            }
        }
        self.reap_stopping(false);
    }

    fn remove_project(&mut self, id: u32) -> Option<Project> {
        let project = self.projects.remove(&id)?;
        self.project_by_dir.remove(&project.dir);
        self.documents.retain(|_, owner| *owner != id);
        self.client_requests.retain(|_, owner| *owner != id);
        self.server_requests.retain(|_, (owner, _)| *owner != id);
        self.progress_tokens.retain(|_, (owner, _)| *owner != id);
        if self.last_active == Some(id) {
            self.last_active = None;
        }
        Some(project)
    }

    fn stop(&mut self, project: Project) {
        self.release(&project);
        let Project {
            child, to_child, ..
        } = project;
        let _ = to_child.send(json!({"jsonrpc": "2.0", "id": SHUTDOWN_ID, "method": "shutdown"}));
        let _ = to_child.send(json!({"jsonrpc": "2.0", "method": "exit"}));
        drop(to_child);
        self.stopping.push((child, Instant::now()));
    }

    fn stop_all(&mut self) {
        let ids: Vec<u32> = self.projects.keys().copied().collect();
        for id in ids {
            if let Some(project) = self.remove_project(id) {
                self.stop(project);
            }
        }
        let deadline = Instant::now() + EXIT_WAIT;
        while !self.stopping.is_empty() && Instant::now() < deadline {
            self.reap_stopping(false);
            thread::sleep(Duration::from_millis(50));
        }
        self.reap_stopping(true);
    }

    fn reap_stopping(&mut self, force: bool) {
        self.stopping.retain_mut(|(child, since)| {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return false;
            }
            if force || since.elapsed() >= STOP_GRACE {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            true
        });
    }

    /// Withdraws what a stopped server left at the client: diagnostics, open requests, watchers.
    fn release(&mut self, project: &Project) {
        for uri in &project.diagnosed {
            self.send(&json!({
                "jsonrpc": "2.0",
                "method": "textDocument/publishDiagnostics",
                "params": {"uri": uri, "diagnostics": []}
            }));
        }
        let message = format!("dbt lsp for {} stopped", project.dir.display());
        for id in project.pending.values() {
            self.reply_error(id.clone(), REQUEST_FAILED, message.clone());
        }
        if project.registrations.is_empty() || self.shutdown_requested {
            return;
        }
        let unregisterations: Vec<Value> = project
            .registrations
            .iter()
            .map(|(id, method)| json!({"id": id, "method": method}))
            .collect();
        // The client's answer finds no entry in `server_requests` and is dropped.
        let mux_id = self.next_server_request_id;
        self.next_server_request_id += 1;
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": mux_id,
            "method": "client/unregisterCapability",
            "params": {"unregisterations": unregisterations}
        }));
    }

    // ---- output ----

    fn send(&mut self, message: &Value) {
        if let Err(error) = rpc::write_message(&mut self.out, message) {
            eprintln!("refmux: cannot write to the client: {error}");
        }
    }

    fn reply(&mut self, id: Value, result: Value) {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    fn reply_error(&mut self, id: Value, code: i64, message: impl Into<String>) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message.into()}
        }));
    }

    fn log(&mut self, kind: u8, text: impl Into<String>) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": "window/logMessage",
            "params": {"type": kind, "message": format!("refmux: {}", text.into())}
        }));
    }

    fn show(&mut self, kind: u8, text: impl Into<String>) {
        let text = text.into();
        self.log(kind, text.clone());
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": "window/showMessage",
            "params": {"type": kind, "message": format!("refmux: {text}")}
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_file_uris_in_nested_command_arguments() {
        let arguments = json!([{"node": "m"}, [{"uri": "file:///p/models/m.sql"}]]);
        assert_eq!(
            first_file_uri(&arguments).as_deref(),
            Some("file:///p/models/m.sql")
        );
        assert_eq!(first_file_uri(&json!(["dbt.show", 3])), None);
    }

    #[test]
    fn keys_ids_by_type() {
        assert_ne!(id_key(&json!(1)), id_key(&json!("1")));
        assert_eq!(token_text(&json!("abc")), "abc");
        assert_eq!(token_text(&json!(42)), "42");
    }
}
