use crate::{convert, handler, Result, Vfs};
use crossbeam_channel::{Receiver, Sender};
use ide::{Analysis, AnalysisHost, Cancelled, VfsPath};
use lsp_server::{ErrorCode, Message, Notification, ReqQueue, Request, RequestId, Response};
use lsp_types::notification::Notification as _;
use lsp_types::{
    notification as notif, request as req, ConfigurationItem, ConfigurationParams,
    PublishDiagnosticsParams, Url,
};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::HashSet;
use std::panic::UnwindSafe;
use std::path::PathBuf;
use std::sync::{Arc, Once, RwLock};
use std::{fs, panic};

const MAX_DIAGNOSTICS_CNT: usize = 128;
const FILTER_FILE_EXTENTION: &str = "nix";
const CONFIG_KEY: &str = "nil";

#[derive(Debug, Default, Deserialize)]
pub struct Config {}

type ReqHandler = fn(&mut State, Response);

pub struct State {
    host: AnalysisHost,
    vfs: Arc<RwLock<Vfs>>,
    opened_files: Arc<RwLock<HashSet<Url>>>,
    workspace_root: Option<PathBuf>,
    req_queue: ReqQueue<(), ReqHandler>,
    sender: Sender<Message>,
    is_shutdown: bool,
    config: Config,
}

impl State {
    pub fn new(responder: Sender<Message>, workspace_root: Option<PathBuf>) -> Self {
        // Vfs root must be absolute.
        let workspace_root = workspace_root.and_then(|root| root.canonicalize().ok());
        let vfs = Vfs::new(workspace_root.clone().unwrap_or_else(|| PathBuf::from("/")));
        Self {
            host: Default::default(),
            vfs: Arc::new(RwLock::new(vfs)),
            opened_files: Default::default(),
            workspace_root,
            req_queue: ReqQueue::default(),
            sender: responder,
            is_shutdown: false,
            config: Config::default(),
        }
    }

    pub fn run(&mut self, lsp_receiver: Receiver<Message>) -> Result<()> {
        if let Some(root) = &self.workspace_root {
            let mut vfs = self.vfs.write().unwrap();
            for entry in ignore::WalkBuilder::new(root).follow_links(false).build() {
                (|| -> Option<()> {
                    let entry = entry.ok()?;
                    if entry
                        .path()
                        .extension()
                        .map_or(true, |ext| ext != FILTER_FILE_EXTENTION)
                    {
                        return None;
                    }

                    let relative_path = entry.path().strip_prefix(root).ok()?;
                    let vpath = VfsPath::from_path(relative_path)?;
                    let text = fs::read_to_string(entry.path()).ok().unwrap_or_default();
                    vfs.set_path_content(vpath, text);
                    Some(())
                })();
            }
            drop(vfs);
            self.apply_vfs_change();
        }

        for msg in &lsp_receiver {
            match msg {
                Message::Request(req) => self.dispatch_request(req),
                Message::Notification(notif) => {
                    if notif.method == notif::Exit::METHOD {
                        return Ok(());
                    }
                    self.dispatch_notification(notif)?;
                }
                Message::Response(resp) => {
                    if let Some(callback) = self.req_queue.outgoing.complete(resp.id.clone()) {
                        callback(self, resp);
                    }
                }
            }
        }

        Err("Channel closed".into())
    }

    fn dispatch_request(&mut self, req: Request) {
        if self.is_shutdown {
            let resp = Response::new_err(
                req.id,
                ErrorCode::InvalidRequest as i32,
                "Shutdown already requested.".into(),
            );
            self.sender.send(resp.into()).unwrap();
            return;
        }

        RequestDispatcher(self, Some(req))
            .on_sync_mut::<req::Shutdown>(|st, ()| {
                st.is_shutdown = true;
                Ok(())
            })
            .on::<req::GotoDefinition>(handler::goto_definition)
            .on::<req::References>(handler::references)
            .on::<req::Completion>(handler::completion)
            .on::<req::SelectionRangeRequest>(handler::selection_range)
            .on::<req::PrepareRenameRequest>(handler::prepare_rename)
            .on::<req::Rename>(handler::rename)
            .on::<req::SemanticTokensFullRequest>(handler::semantic_token_full)
            .on::<req::SemanticTokensRangeRequest>(handler::semantic_token_range)
            .on::<req::HoverRequest>(handler::hover)
            .finish();
    }

    fn dispatch_notification(&mut self, notif: Notification) -> Result<()> {
        NotificationDispatcher(self, Some(notif))
            .on_sync_mut::<notif::DidOpenTextDocument>(|st, params| {
                let uri = &params.text_document.uri;
                st.opened_files.write().unwrap().insert(uri.clone());
                st.set_vfs_file_content(uri, params.text_document.text)?;
                Ok(())
            })?
            .on_sync_mut::<notif::DidCloseTextDocument>(|st, params| {
                // N.B. Don't clear text here.
                st.opened_files
                    .write()
                    .unwrap()
                    .remove(&params.text_document.uri);
                Ok(())
            })?
            .on_sync_mut::<notif::DidChangeTextDocument>(|st, params| {
                let mut vfs = st.vfs.write().unwrap();
                if let Ok(file) = vfs.file_for_uri(&params.text_document.uri) {
                    for change in params.content_changes {
                        if let Some((_, range)) = change
                            .range
                            .and_then(|range| convert::from_range(&vfs, file, range).ok())
                        {
                            let _ = vfs.change_file_content(file, range, &change.text);
                        }
                    }
                    drop(vfs);
                    st.apply_vfs_change();
                }
                Ok(())
            })?
            .on_sync_mut::<notif::DidChangeConfiguration>(|st, _params| {
                // As stated in https://github.com/microsoft/language-server-protocol/issues/676,
                // this notification's parameters should be ignored and the actual config queried separately.
                st.send_request::<req::WorkspaceConfiguration>(
                    ConfigurationParams {
                        items: vec![ConfigurationItem {
                            scope_uri: None,
                            section: Some(CONFIG_KEY.into()),
                        }],
                    },
                    |st, resp| {
                        let ret = match resp.error {
                            None => Ok(resp
                                .result
                                .and_then(|mut v| Some(v.get_mut(0)?.take()))
                                .unwrap_or_default()),
                            Some(err) => Err(format!(
                                "LSP error {}: {}, data: {:?}",
                                err.code, err.message, err.data
                            )),
                        };
                        match ret {
                            Ok(v) => {
                                tracing::info!("Updating config: {:?}", v);
                                st.update_config(v);
                            }
                            Err(err) => tracing::error!("Failed to update config: {}", err),
                        }
                    },
                );
                Ok(())
            })?
            .finish()
    }

    fn send_request<R: req::Request>(
        &mut self,
        params: R::Params,
        callback: fn(&mut Self, Response),
    ) {
        let req = self
            .req_queue
            .outgoing
            .register(R::METHOD.into(), params, callback);
        self.sender.send(req.into()).unwrap();
    }

    fn send_notification<N: notif::Notification>(&self, params: N::Params) {
        self.sender
            .send(Notification::new(N::METHOD.into(), params).into())
            .unwrap();
    }

    fn update_config(&mut self, _config: serde_json::Value) {
        // No-op.
        let _ = &mut self.config;
    }

    fn snapshot(&self) -> StateSnapshot {
        StateSnapshot {
            analysis: self.host.snapshot(),
            vfs: Arc::clone(&self.vfs),
        }
    }

    fn set_vfs_file_content(&mut self, uri: &Url, text: String) -> Result<()> {
        self.vfs.write().unwrap().set_uri_content(uri, text)?;
        self.apply_vfs_change();
        Ok(())
    }

    fn apply_vfs_change(&mut self) {
        let mut vfs = self.vfs.write().unwrap();
        let change = vfs.take_change();
        let file_changes = change
            .file_changes
            .iter()
            .map(|(file, text)| (*file, !text.is_empty()))
            .collect::<Vec<_>>();
        tracing::debug!("Change: {:?}", change);
        self.host.apply_change(change);

        let snap = self.host.snapshot();
        let opened_files = self.opened_files.read().unwrap();
        for (file, has_text) in file_changes {
            let uri = vfs.uri_for_file(file);
            if !opened_files.contains(&uri) {
                continue;
            }

            // TODO: Error is ignored.
            let diagnostics = has_text
                .then(|| {
                    let mut diags = snap.diagnostics(file).ok()?;
                    diags.truncate(MAX_DIAGNOSTICS_CNT);
                    Some(convert::to_diagnostics(&vfs, file, &diags))
                })
                .flatten()
                .unwrap_or_default();
            self.send_notification::<notif::PublishDiagnostics>(PublishDiagnosticsParams {
                uri,
                diagnostics,
                version: None,
            });
        }
    }
}

#[must_use = "RequestDispatcher::finish not called"]
struct RequestDispatcher<'s>(&'s mut State, Option<Request>);

impl<'s> RequestDispatcher<'s> {
    fn on_sync_mut<R: req::Request>(
        mut self,
        f: fn(&mut State, R::Params) -> Result<R::Result>,
    ) -> Self {
        if matches!(&self.1, Some(notif) if notif.method == R::METHOD) {
            let req = self.1.take().unwrap();
            let ret = match serde_json::from_value::<R::Params>(req.params) {
                Ok(params) => result_to_response(req.id, f(self.0, params)),
                Err(err) => Ok(Response::new_err(
                    req.id,
                    ErrorCode::InvalidParams as i32,
                    err.to_string(),
                )),
            };
            if let Ok(resp) = ret {
                self.0.sender.send(resp.into()).unwrap();
            }
        }
        self
    }

    fn on<R: req::Request>(mut self, f: fn(StateSnapshot, R::Params) -> Result<R::Result>) -> Self
    where
        R::Params: UnwindSafe,
    {
        if matches!(&self.1, Some(notif) if notif.method == R::METHOD) {
            let req = self.1.take().unwrap();
            let ret = match serde_json::from_value::<R::Params>(req.params) {
                Ok(params) => {
                    let snap = self.0.snapshot();
                    result_to_response(req.id, with_catch_unwind(R::METHOD, || f(snap, params)))
                }
                Err(err) => Ok(Response::new_err(
                    req.id,
                    ErrorCode::InvalidParams as i32,
                    err.to_string(),
                )),
            };
            if let Ok(resp) = ret {
                self.0.sender.send(resp.into()).unwrap();
            }
        }
        self
    }

    fn finish(self) {
        if let Some(req) = self.1 {
            let resp = Response::new_err(req.id, ErrorCode::MethodNotFound as _, String::new());
            self.0.sender.send(resp.into()).unwrap();
        }
    }
}

#[must_use = "NotificationDispatcher::finish not called"]
struct NotificationDispatcher<'s>(&'s mut State, Option<Notification>);

impl<'s> NotificationDispatcher<'s> {
    fn on_sync_mut<N: notif::Notification>(
        mut self,
        f: fn(&mut State, N::Params) -> Result<()>,
    ) -> Result<Self> {
        if matches!(&self.1, Some(notif) if notif.method == N::METHOD) {
            let params =
                serde_json::from_value::<N::Params>(self.1.take().unwrap().params).unwrap();
            f(self.0, params)?;
        }
        Ok(self)
    }

    fn finish(self) -> Result<()> {
        if let Some(notif) = self.1 {
            if !notif.method.starts_with("$/") {
                tracing::error!("Unhandled notification: {:?}", notif);
            }
        }
        Ok(())
    }
}

fn with_catch_unwind<T>(ctx: &str, f: impl FnOnce() -> Result<T> + UnwindSafe) -> Result<T> {
    static INSTALL_PANIC_HOOK: Once = Once::new();
    thread_local! {
        static PANIC_LOCATION: Cell<String> = Cell::new(String::new());
    }

    INSTALL_PANIC_HOOK.call_once(|| {
        let old_hook = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if let Some(loc) = info.location() {
                PANIC_LOCATION.with(|inner| {
                    inner.set(loc.to_string());
                });
            }
            old_hook(info);
        }))
    });

    match panic::catch_unwind(f) {
        Ok(ret) => ret,
        Err(payload) => {
            let reason = payload
                .downcast_ref::<String>()
                .map(|s| &**s)
                .or_else(|| payload.downcast_ref::<&str>().map(|s| &**s))
                .unwrap_or("unknown");
            let mut loc = PANIC_LOCATION.with(|inner| inner.take());
            if loc.is_empty() {
                loc = "unknown".into();
            }
            let msg = format!("Request handler of {} panicked at {}: {}", ctx, loc, reason);
            Err(msg.into())
        }
    }
}

fn result_to_response(id: RequestId, ret: Result<impl Serialize>) -> Result<Response, Cancelled> {
    match ret {
        Ok(ret) => Ok(Response::new_ok(id, ret)),
        Err(err) => match err.downcast::<Cancelled>() {
            Ok(cancelled) => Err(*cancelled),
            Err(err) => Ok(Response::new_err(
                id,
                ErrorCode::InternalError as i32,
                err.to_string(),
            )),
        },
    }
}

#[derive(Debug)]
pub struct StateSnapshot {
    pub(crate) analysis: Analysis,
    vfs: Arc<RwLock<Vfs>>,
}

impl StateSnapshot {
    pub(crate) fn vfs(&self) -> impl std::ops::Deref<Target = Vfs> + '_ {
        self.vfs.read().unwrap()
    }
}
