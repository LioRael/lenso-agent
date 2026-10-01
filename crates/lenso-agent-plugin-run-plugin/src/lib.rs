//! Host-bound task isolation. Existing Loop owns execution; this Plugin owns
//! per-call admission, transient history, and narrowed forwarding across Kernels.
use futures::future::LocalBoxFuture;
use lenso_agent_native_support::FiniteOutputStream;
use lenso_capability_agent_artifact as artifact;
use lenso_capability_agent_context_compaction as compaction;
use lenso_capability_agent_memory as memory;
use lenso_capability_agent_model as model;
use lenso_capability_agent_prompt as prompt;
use lenso_capability_agent_session as session;
use lenso_capability_agent_tools as tools;
use lenso_kernel::{InvocationContext, NativeRequestFuture, RuntimeFailure, StreamEvent};
use std::{cell::RefCell, collections::BTreeSet, rc::Rc};

#[derive(Clone, Debug)]
pub struct Bindings {
    pub model: model::ModelClient,
    pub tools: tools::ToolsClient,
    pub prompt: prompt::PromptClient,
    /// Present only after explicit, owner/project-verified write admission.
    pub session: Option<session::SessionClient>,
    pub session_id: String,
    pub model_id: String,
    pub max_calls: u32,
    pub max_output: u64,
    pub input_ceiling: u64,
    pub allowed_tools: BTreeSet<String>,
    pub usage: Rc<RefCell<Usage>>,
}
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Usage {
    pub calls: u32,
    pub completed_calls: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
}
#[derive(Clone, Debug, Default)]
struct TransientSession {
    opened: bool,
    events: Vec<session::ReadSessionResponseEventsItem>,
    bytes: usize,
}
#[lenso::plugin]
#[derive(Clone, Debug)]
struct RunBoundary {
    bindings: Option<Bindings>,
    transient: Rc<RefCell<TransientSession>>,
}
struct CancelCall {
    token: lenso_kernel::CancellationToken,
    complete: bool,
}
impl Drop for CancelCall {
    fn drop(&mut self) {
        if !self.complete {
            self.token.cancel();
        }
    }
}
fn failure(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: detail.into(),
    }
}
impl RunBoundary {
    fn bound(&self) -> Result<Bindings, RuntimeFailure> {
        self.bindings
            .clone()
            .ok_or_else(|| failure("run boundary has no Host admission"))
    }
    fn reply<C: lenso_kernel::RequestCapability>(
        &self,
        result: Result<C::Response, C::DomainError>,
    ) -> NativeRequestFuture<C> {
        let bound = self.bound();
        Box::pin(async move {
            bound?;
            Ok(result)
        })
    }
}
pub fn factory(bindings: Bindings) -> impl lenso_native_adapter::NativePluginFactory {
    lenso_native_adapter::ConfiguredPluginFactory::<RunBoundary, _>::new(move |plugin| {
        plugin.bindings = Some(bindings.clone());
        Ok(())
    })
}
pub fn link() {
    lenso_native_adapter::link_plugin::<RunBoundary>();
}

// Exact clients are captured from the retained parent Generation's resolved
// dependencies. No discovery, registry fallback or credential enters arguments.
impl RunBoundary {
    fn model_catalog(
        &self,
        context: InvocationContext,
        request: model::CatalogRequest,
    ) -> NativeRequestFuture<model::ModelCatalog> {
        let bound = self.bound();
        Box::pin(async move {
            let b = bound?;
            b.model
                .catalog_with_context(context, request)
                .await
                .map(Ok)
                .map_err(|e| failure(format!("model catalog: {e:?}")))
        })
    }
    #[expect(
        clippy::too_many_lines,
        reason = "one bounded Model invocation and terminal usage admission"
    )]
    fn complete(
        &self,
        context: InvocationContext,
        request: model::CompleteOpen,
    ) -> LocalBoxFuture<'static, Result<FiniteOutputStream, model::ModelCompleteInvocationError>>
    {
        let bound = self.bound();
        Box::pin(async move {
            let b = bound.map_err(model::ModelCompleteInvocationError::Runtime)?;
            let mut call = CancelCall {
                token: context.cancellation(),
                complete: false,
            };
            if request.model != b.model_id
                || request.max_output_tokens <= 0
                || u64::try_from(request.max_output_tokens).unwrap_or(u64::MAX) > b.max_output
                || request
                    .tools
                    .iter()
                    .any(|t| !b.allowed_tools.contains(&t.name))
            {
                return Err(model::ModelCompleteInvocationError::Runtime(failure(
                    "run model/tool admission denied",
                )));
            }
            {
                let mut usage = b.usage.borrow_mut();
                if usage.calls >= b.max_calls {
                    return Err(model::ModelCompleteInvocationError::Runtime(failure(
                        "reserved model calls exhausted",
                    )));
                }
                usage.calls += 1;
            }
            // Reserve all calls before dispatch in Console; consume one here
            // before every real open, including any retry by the existing Loop.
            let stream = b
                .model
                .complete_with_context(context.clone(), request.clone())
                .await?;
            stream
                .close_send()
                .await
                .map_err(model::ModelCompleteInvocationError::Runtime)?;
            let mut messages = Vec::new();
            let mut bytes = 0usize;
            let mut measured = None;
            loop {
                let cancellation = context.cancellation();
                let event = tokio_select_receive(&stream, &cancellation)
                    .await
                    .map_err(model::ModelCompleteInvocationError::Runtime)?;
                match event {
                    StreamEvent::Message(message) => {
                        bytes = bytes.saturating_add(
                            message.text.len() + message.arguments_json.as_str().len(),
                        );
                        if bytes > 1_048_576 {
                            stream.cancel();
                            return Err(model::ModelCompleteInvocationError::Runtime(failure(
                                "bounded model output exceeded",
                            )));
                        }
                        if message.kind == model::CompleteMessageKind::Usage {
                            let input = message.input_tokens.parse::<u64>().map_err(|_| {
                                model::ModelCompleteInvocationError::Runtime(failure(
                                    "missing input usage",
                                ))
                            })?;
                            let output = message.output_tokens.parse::<u64>().map_err(|_| {
                                model::ModelCompleteInvocationError::Runtime(failure(
                                    "missing output usage",
                                ))
                            })?;
                            if input > b.input_ceiling
                                || output > u64::try_from(request.max_output_tokens).unwrap_or(0)
                            {
                                stream.cancel();
                                return Err(model::ModelCompleteInvocationError::Runtime(failure(
                                    "model usage exceeds reservation",
                                )));
                            }
                            measured = Some((input, output));
                        }
                        messages.push(message);
                    }
                    StreamEvent::PeerHalfClosed => {}
                    StreamEvent::Terminal(Ok(())) => break,
                    StreamEvent::Terminal(Err(e)) => {
                        return Err(model::ModelCompleteInvocationError::Domain(e));
                    }
                }
            }
            let (input, output) = measured.ok_or_else(|| {
                model::ModelCompleteInvocationError::Runtime(failure("run requires final usage"))
            })?;
            {
                let mut usage = b.usage.borrow_mut();
                usage.input_tokens = usage.input_tokens.checked_add(input).ok_or_else(|| {
                    model::ModelCompleteInvocationError::Runtime(failure("usage overflow"))
                })?;
                usage.output_tokens = usage.output_tokens.checked_add(output).ok_or_else(|| {
                    model::ModelCompleteInvocationError::Runtime(failure("usage overflow"))
                })?;
                usage.completed_calls += 1;
            }
            call.complete = true;
            Ok(FiniteOutputStream::successful(
                model::CAPABILITY_ID,
                messages,
            ))
        })
    }
}
async fn tokio_select_receive(
    stream: &lenso_kernel::NativeStream<model::ModelComplete>,
    cancellation: &lenso_kernel::CancellationToken,
) -> Result<StreamEvent<model::CompleteMessage, model::CompleteError>, RuntimeFailure> {
    use futures::{
        FutureExt,
        future::{Either, select},
    };
    match select(
        stream.receive().boxed_local(),
        cancellation.cancelled().boxed_local(),
    )
    .await
    {
        Either::Left((event, _)) => event,
        Either::Right(_) => {
            stream.cancel();
            Err(failure("run cancelled"))
        }
    }
}

impl RunBoundary {
    fn tools_catalog(
        &self,
        context: InvocationContext,
        request: tools::CatalogRequest,
    ) -> NativeRequestFuture<tools::ToolsCatalog> {
        let bound = self.bound();
        Box::pin(async move {
            let b = bound?;
            let mut catalog = b
                .tools
                .catalog_with_context(context, request)
                .await
                .map_err(|e| failure(format!("tool catalog: {e:?}")))?;
            catalog.tools.retain(|t| b.allowed_tools.contains(&t.name));
            Ok(Ok(catalog))
        })
    }
    fn execute(
        &self,
        context: InvocationContext,
        request: tools::ExecuteRequest,
    ) -> NativeRequestFuture<tools::ToolsExecute> {
        let bound = self.bound();
        Box::pin(async move {
            let b = bound?;
            if !b.allowed_tools.contains(&request.name) {
                return Err(failure("tool not admitted"));
            }
            b.tools
                .execute_with_context(context, request)
                .await
                .map(Ok)
                .map_err(|e| failure(format!("tool execution: {e:?}")))
        })
    }
    fn execute_stream(
        &self,
        context: InvocationContext,
        request: tools::ExecuteStreamRequest,
    ) -> LocalBoxFuture<'static, Result<FiniteOutputStream, tools::ToolsExecuteStreamInvocationError>>
    {
        let bound = self.bound();
        Box::pin(async move {
            let b = bound.map_err(tools::ToolsExecuteStreamInvocationError::Runtime)?;
            let mut call = CancelCall {
                token: context.cancellation(),
                complete: false,
            };
            if !b.allowed_tools.contains(&request.name) {
                return Err(tools::ToolsExecuteStreamInvocationError::Runtime(failure(
                    "tool not admitted",
                )));
            }
            let stream = b
                .tools
                .execute_stream_with_context(context.clone(), request)
                .await?;
            stream
                .close_send()
                .await
                .map_err(tools::ToolsExecuteStreamInvocationError::Runtime)?;
            let mut messages = Vec::new();
            let mut bytes = 0usize;
            loop {
                match stream
                    .receive()
                    .await
                    .map_err(tools::ToolsExecuteStreamInvocationError::Runtime)?
                {
                    StreamEvent::Message(message) => {
                        bytes += serde_json::to_vec(&message)
                            .map_err(|e| {
                                tools::ToolsExecuteStreamInvocationError::Runtime(failure(
                                    e.to_string(),
                                ))
                            })?
                            .len();
                        if bytes > 1_048_576 {
                            stream.cancel();
                            return Err(tools::ToolsExecuteStreamInvocationError::Runtime(
                                failure("tool output exceeds task bound"),
                            ));
                        }
                        messages.push(message);
                    }
                    StreamEvent::PeerHalfClosed => {}
                    StreamEvent::Terminal(Ok(())) => break,
                    StreamEvent::Terminal(Err(e)) => {
                        return Err(tools::ToolsExecuteStreamInvocationError::Domain(e));
                    }
                }
            }
            call.complete = true;
            Ok(FiniteOutputStream::successful(
                tools::CAPABILITY_ID,
                messages,
            ))
        })
    }
}
impl RunBoundary {
    fn assemble(
        &self,
        context: InvocationContext,
        request: prompt::AssembleRequest,
    ) -> NativeRequestFuture<prompt::Prompt> {
        let bound = self.bound();
        Box::pin(async move {
            let b = bound?;
            b.prompt
                .assemble_with_context(context, request)
                .await
                .map(Ok)
                .map_err(|e| failure(format!("prompt: {e:?}")))
        })
    }
}

impl RunBoundary {
    fn open(
        &self,
        context: InvocationContext,
        request: session::OpenSessionRequest,
    ) -> NativeRequestFuture<session::SessionOpen> {
        let bound = self.bound();
        let state = self.transient.clone();
        Box::pin(async move {
            let b = bound?;
            if request.session_id.as_deref() != Some(&b.session_id)
                || request.create_session_id.is_some()
            {
                return Ok(Err(session::OpenError::PermissionDenied));
            }
            if let Some(client) = b.session {
                return client
                    .open_with_context(context, request)
                    .await
                    .map(Ok)
                    .map_err(|e| failure(format!("explicit session open: {e:?}")));
            }
            let mut state = state.borrow_mut();
            let created = !state.opened;
            state.opened = true;
            Ok(Ok(session::OpenSessionResponse {
                created,
                session_id: b.session_id,
                revision: state.events.len().to_string(),
            }))
        })
    }
    fn append(
        &self,
        context: InvocationContext,
        request: session::AppendSessionRequest,
    ) -> NativeRequestFuture<session::SessionAppend> {
        let bound = self.bound();
        let state = self.transient.clone();
        Box::pin(async move {
            let b = bound?;
            if request.session_id != b.session_id {
                return Ok(Err(session::AppendError::PermissionDenied));
            }
            if let Some(client) = b.session {
                return client
                    .append_with_context(context, request)
                    .await
                    .map(Ok)
                    .map_err(|e| failure(format!("explicit session append: {e:?}")));
            }
            let mut state = state.borrow_mut();
            if !state.opened || request.expected_revision != state.events.len().to_string() {
                return Ok(Err(session::AppendError::PermissionDenied));
            }
            let bytes = serde_json::to_vec(&request.events)
                .map_err(|e| failure(e.to_string()))?
                .len();
            if state.bytes + bytes > 4_194_304 || state.events.len() + request.events.len() > 4096 {
                return Err(failure("transient session bound exceeded"));
            }
            for event in request.events {
                let mut event = serde_json::to_value(event).map_err(|e| failure(e.to_string()))?;
                event["revision"] = (state.events.len() + 1).to_string().into();
                state
                    .events
                    .push(serde_json::from_value(event).map_err(|e| failure(e.to_string()))?);
            }
            state.bytes += bytes;
            Ok(Ok(session::AppendSessionResponse {
                revision: state.events.len().to_string(),
            }))
        })
    }
    fn session_read(
        &self,
        context: InvocationContext,
        request: session::ReadSessionRequest,
    ) -> NativeRequestFuture<session::SessionRead> {
        let bound = self.bound();
        let state = self.transient.clone();
        Box::pin(async move {
            let b = bound?;
            if request.session_id != b.session_id {
                return Ok(Err(session::ReadError::PermissionDenied));
            }
            if let Some(client) = b.session {
                return client
                    .read_with_context(context, request)
                    .await
                    .map(Ok)
                    .map_err(|e| failure(format!("explicit session read: {e:?}")));
            }
            let cursor = request
                .after_revision
                .parse::<usize>()
                .map_err(|_| failure("invalid transient cursor"))?;
            let state = state.borrow();
            if !state.opened || cursor > state.events.len() || !(1..=1000).contains(&request.limit)
            {
                return Ok(Err(session::ReadError::InvalidCursor));
            }
            Ok(Ok(session::ReadSessionResponse {
                session_id: b.session_id,
                revision: state.events.len().to_string(),
                title: None,
                title_revision: None,
                events: state
                    .events
                    .iter()
                    .skip(cursor)
                    .take(usize::try_from(request.limit).unwrap_or(0))
                    .cloned()
                    .collect(),
            }))
        })
    }
    fn list(
        &self,
        _: InvocationContext,
        _: session::ListSessionsRequest,
    ) -> NativeRequestFuture<session::SessionList> {
        self.reply::<session::SessionList>(Ok(session::ListSessionsResponse { sessions: vec![] }))
    }
    fn rename(
        &self,
        _: InvocationContext,
        _: session::RenameSessionRequest,
    ) -> NativeRequestFuture<session::SessionRename> {
        self.reply::<session::SessionRename>(Err(session::RenameError::PermissionDenied))
    }
}

// Background tasks do not read/write shared memory, persist artifacts, or invoke
// an unmetered compaction Model. Limits fail visibly instead of hiding extra calls.
impl RunBoundary {
    fn recall(
        &self,
        _: InvocationContext,
        _: memory::RecallRequest,
    ) -> NativeRequestFuture<memory::MemoryRecall> {
        self.reply::<memory::MemoryRecall>(Ok(memory::RecallResponse { items: vec![] }))
    }
    fn observe(
        &self,
        _: InvocationContext,
        _: memory::ObserveRequest,
    ) -> NativeRequestFuture<memory::MemoryObserve> {
        self.reply::<memory::MemoryObserve>(Ok(memory::ObserveResponse { memory_ids: vec![] }))
    }
    fn remember(
        &self,
        _: InvocationContext,
        _: memory::RememberRequest,
    ) -> NativeRequestFuture<memory::MemoryRemember> {
        self.reply::<memory::MemoryRemember>(Err(memory::RememberError::InvalidRequest))
    }
    fn forget(
        &self,
        _: InvocationContext,
        _: memory::ForgetRequest,
    ) -> NativeRequestFuture<memory::MemoryForget> {
        self.reply::<memory::MemoryForget>(Err(memory::ForgetError::InvalidRequest))
    }
}
impl RunBoundary {
    fn compact(
        &self,
        _: InvocationContext,
        _: compaction::CompactRequest,
    ) -> NativeRequestFuture<compaction::ContextCompaction> {
        self.reply::<compaction::ContextCompaction>(Err(compaction::CompactError::ContextTooLarge))
    }
}
impl RunBoundary {
    fn put(
        &self,
        _: InvocationContext,
        _: artifact::PutRequest,
    ) -> NativeRequestFuture<artifact::ArtifactPut> {
        self.reply::<artifact::ArtifactPut>(Err(artifact::PutError::PermissionDenied))
    }
    fn artifact_read(
        &self,
        _: InvocationContext,
        _: artifact::ReadRequest,
    ) -> NativeRequestFuture<artifact::ArtifactRead> {
        self.reply::<artifact::ArtifactRead>(Err(artifact::ReadError::PermissionDenied))
    }
}

// These existing roles share operation names. One generic inherent method keeps
// the generated lowering typed without hand-written factories or endpoints.
trait CatalogRequest {
    type Role: lenso_kernel::RequestCapability;
    fn invoke(
        self,
        plugin: &RunBoundary,
        context: InvocationContext,
    ) -> NativeRequestFuture<Self::Role>;
}
impl CatalogRequest for model::CatalogRequest {
    type Role = model::ModelCatalog;
    fn invoke(self, p: &RunBoundary, c: InvocationContext) -> NativeRequestFuture<Self::Role> {
        p.model_catalog(c, self)
    }
}
impl CatalogRequest for tools::CatalogRequest {
    type Role = tools::ToolsCatalog;
    fn invoke(self, p: &RunBoundary, c: InvocationContext) -> NativeRequestFuture<Self::Role> {
        p.tools_catalog(c, self)
    }
}
trait ReadRequest {
    type Role: lenso_kernel::RequestCapability;
    fn invoke(
        self,
        plugin: &RunBoundary,
        context: InvocationContext,
    ) -> NativeRequestFuture<Self::Role>;
}
impl ReadRequest for session::ReadSessionRequest {
    type Role = session::SessionRead;
    fn invoke(self, p: &RunBoundary, c: InvocationContext) -> NativeRequestFuture<Self::Role> {
        p.session_read(c, self)
    }
}
impl ReadRequest for artifact::ReadRequest {
    type Role = artifact::ArtifactRead;
    fn invoke(self, p: &RunBoundary, c: InvocationContext) -> NativeRequestFuture<Self::Role> {
        p.artifact_read(c, self)
    }
}
#[lenso::provides(
    model::Model,
    tools::Tools,
    prompt::Prompt,
    session::Session,
    memory::Memory,
    compaction::ContextCompaction,
    artifact::Artifact
)]
impl RunBoundary {
    fn catalog<R: CatalogRequest>(
        &self,
        context: InvocationContext,
        request: R,
    ) -> NativeRequestFuture<R::Role> {
        request.invoke(self, context)
    }
    fn read<R: ReadRequest>(
        &self,
        context: InvocationContext,
        request: R,
    ) -> NativeRequestFuture<R::Role> {
        request.invoke(self, context)
    }
}
