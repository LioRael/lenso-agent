//! Terminal projection for durable Agent Sessions.

use std::fmt::Write as _;

use futures::future::ready;
use lenso::prelude::*;
use lenso_agent_native_support::FiniteOutputStream;
use lenso_capability_agent_session as session_contract;
use lenso_capability_terminal_command_provider as command_contract;
use lenso_capability_terminal_command_provider::{
    CatalogRequest, CatalogResponse, CommandDefinition, CommandParameter, CommandProviderCatalog,
    CommandProviderExecuteInvocationError, CommandProviderProvider, ContentType, ExecuteError,
    ExecuteMessage, ExecuteOpen, OutputFormat, OutputKind, ParameterKind,
};
use lenso_kernel::{InvocationContext, NativeStreamSession, RuntimeFailure};

const LIST_COMMAND: &str = "agent.session.list";
const SHOW_COMMAND: &str = "agent.session.show";
const DEFAULT_LIST_LIMIT: i64 = 20;
const DEFAULT_SHOW_LIMIT: i64 = 100;
const MAX_OUTPUT_BYTES: usize = 1_048_576;

#[lenso::plugin]
#[derive(Clone, Debug)]
struct SessionTerminalCommands {
    sessions: Port<session_contract::SessionClient>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArguments {
    #[serde(default)]
    limit: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ShowArguments {
    session_id: String,
    #[serde(default)]
    after_revision: Option<String>,
    #[serde(default)]
    limit: Option<String>,
}

#[lenso::provides(command_contract::CommandProvider)]
impl CommandProviderProvider for SessionTerminalCommands {
    fn catalog(
        &self,
        _context: InvocationContext,
        _request: CatalogRequest,
    ) -> lenso_kernel::NativeRequestFuture<CommandProviderCatalog> {
        Box::pin(ready(Ok(Ok(CatalogResponse {
            commands: command_catalog(),
        }))))
    }

    fn execute(
        &self,
        context: InvocationContext,
        request: ExecuteOpen,
    ) -> futures::future::LocalBoxFuture<
        'static,
        Result<Box<dyn NativeStreamSession>, CommandProviderExecuteInvocationError>,
    > {
        let sessions = self.sessions.clone();
        Box::pin(async move {
            let message = match request.id.as_str() {
                LIST_COMMAND => execute_list(&sessions, &context, &request).await,
                SHOW_COMMAND => execute_show(&sessions, &context, &request).await,
                _ => Err(PluginError::domain(ExecuteError::NotFound)),
            }
            .map_err(|error| match error {
                PluginError::Domain(error) => CommandProviderExecuteInvocationError::Domain(error),
                PluginError::Runtime(error) => {
                    CommandProviderExecuteInvocationError::Runtime(error)
                }
            })?;
            Ok(Box::new(FiniteOutputStream::successful(
                command_contract::CAPABILITY_ID,
                vec![message],
            )) as Box<dyn NativeStreamSession>)
        })
    }
}

fn command_catalog() -> Vec<CommandDefinition> {
    vec![
        CommandDefinition {
            id: LIST_COMMAND.to_owned(),
            path: vec!["sessions".to_owned(), "list".to_owned()],
            summary: "List durable Agent sessions".to_owned(),
            description: "List the most recently updated sessions from the bound Session provider."
                .to_owned(),
            parameters: vec![option_parameter(
                "limit",
                "limit",
                Some("n"),
                "LIMIT",
                "Maximum number of sessions to return (1-100).",
            )],
            output_formats: vec![OutputFormat::Text, OutputFormat::Json],
        },
        CommandDefinition {
            id: SHOW_COMMAND.to_owned(),
            path: vec!["sessions".to_owned(), "show".to_owned()],
            summary: "Show one durable Agent session".to_owned(),
            description:
                "Read one session and its durable event log from the bound Session provider."
                    .to_owned(),
            parameters: vec![
                CommandParameter {
                    id: "session_id".to_owned(),
                    kind: ParameterKind::Positional,
                    long: None,
                    short: None,
                    value_name: Some(Some("SESSION_ID".to_owned())),
                    description: "Stable session identifier.".to_owned(),
                    required: true,
                    multiple: false,
                    choices: Vec::new(),
                },
                option_parameter(
                    "after_revision",
                    "after",
                    None,
                    "REVISION",
                    "Return events after this revision (default: 0).",
                ),
                option_parameter(
                    "limit",
                    "limit",
                    Some("n"),
                    "LIMIT",
                    "Maximum number of events to return (1-1000).",
                ),
            ],
            output_formats: vec![OutputFormat::Text, OutputFormat::Json],
        },
    ]
}

fn option_parameter(
    id: &str,
    long: &str,
    short: Option<&str>,
    value_name: &str,
    description: &str,
) -> CommandParameter {
    CommandParameter {
        id: id.to_owned(),
        kind: ParameterKind::Option,
        long: Some(Some(long.to_owned())),
        short: short.map(|short| Some(short.to_owned())),
        value_name: Some(Some(value_name.to_owned())),
        description: description.to_owned(),
        required: false,
        multiple: false,
        choices: Vec::new(),
    }
}

async fn execute_list(
    sessions: &session_contract::SessionClient,
    context: &InvocationContext,
    request: &ExecuteOpen,
) -> PluginResult<ExecuteMessage, ExecuteError> {
    let arguments: ListArguments = parse_arguments(request)?;
    let limit = parse_limit(arguments.limit.as_deref(), DEFAULT_LIST_LIMIT, 100)?;
    let response = sessions
        .list_with_context(
            context.clone(),
            session_contract::ListSessionsRequest { limit },
        )
        .await
        .map_err(map_list_error)?;
    let rendered = match request.output_format {
        OutputFormat::Json => serialize_json(&response)?,
        OutputFormat::Text => render_session_list(&response),
    };
    output_message(request, rendered)
}

async fn execute_show(
    sessions: &session_contract::SessionClient,
    context: &InvocationContext,
    request: &ExecuteOpen,
) -> PluginResult<ExecuteMessage, ExecuteError> {
    let arguments: ShowArguments = parse_arguments(request)?;
    if arguments.session_id.trim().is_empty() || arguments.session_id.len() > 128 {
        return Err(PluginError::domain(ExecuteError::InvalidArguments));
    }
    let after_revision = arguments
        .after_revision
        .as_deref()
        .unwrap_or("0")
        .to_owned();
    if !valid_uint64(&after_revision) {
        return Err(PluginError::domain(ExecuteError::InvalidArguments));
    }
    let limit = parse_limit(arguments.limit.as_deref(), DEFAULT_SHOW_LIMIT, 1_000)?;
    let response = sessions
        .read_with_context(
            context.clone(),
            session_contract::ReadSessionRequest {
                session_id: arguments.session_id,
                after_revision,
                limit,
            },
        )
        .await
        .map_err(map_read_error)?;
    let rendered = match request.output_format {
        OutputFormat::Json => serialize_json(&response)?,
        OutputFormat::Text => render_session(&response),
    };
    output_message(request, rendered)
}

fn parse_arguments<T: serde::de::DeserializeOwned>(
    request: &ExecuteOpen,
) -> PluginResult<T, ExecuteError> {
    serde_json::from_str(request.arguments_json.as_str())
        .map_err(|_| PluginError::domain(ExecuteError::InvalidArguments))
}

fn parse_limit(value: Option<&str>, default: i64, max: i64) -> PluginResult<i64, ExecuteError> {
    let value = value.map_or(Ok(default), str::parse::<i64>);
    match value {
        Ok(value) if (1..=max).contains(&value) => Ok(value),
        _ => Err(PluginError::domain(ExecuteError::InvalidArguments)),
    }
}

fn valid_uint64(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 20
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.parse::<u64>().is_ok()
}

fn serialize_json<T: serde::Serialize>(value: &T) -> PluginResult<String, ExecuteError> {
    serde_json::to_string_pretty(value).map_err(|error| {
        PluginError::runtime(RuntimeFailure::PluginFailure {
            detail: format!("failed to encode terminal command result: {error}"),
        })
    })
}

fn output_message(
    request: &ExecuteOpen,
    content: String,
) -> PluginResult<ExecuteMessage, ExecuteError> {
    if content.len() > MAX_OUTPUT_BYTES {
        return Err(PluginError::domain(ExecuteError::OutputLimitExceeded));
    }
    Ok(ExecuteMessage {
        kind: OutputKind::Result,
        content_type: match request.output_format {
            OutputFormat::Text => ContentType::Text,
            OutputFormat::Json => ContentType::Json,
        },
        content,
    })
}

fn render_session_list(response: &session_contract::ListSessionsResponse) -> String {
    if response.sessions.is_empty() {
        return "No sessions.\n".to_owned();
    }
    let mut output = String::from("SESSION ID\tREVISION\tUPDATED\tTITLE\n");
    for session in &response.sessions {
        writeln!(
            output,
            "{}\t{}\t{}\t{}",
            session.session_id,
            session.revision,
            session.updated_at,
            session.title.as_deref().unwrap_or("-")
        )
        .expect("writing to a String cannot fail");
    }
    output
}

fn render_session(response: &session_contract::ReadSessionResponse) -> String {
    let mut output = format!(
        "Session: {}\nRevision: {}\nTitle: {}\n\n",
        response.session_id,
        response.revision,
        response.title.as_deref().unwrap_or("-")
    );
    if response.events.is_empty() {
        output.push_str("No events in the selected revision range.\n");
        return output;
    }
    for event in &response.events {
        let kind = serde_json::to_string(&event.kind)
            .unwrap_or_else(|_| "\"unknown\"".to_owned())
            .trim_matches('"')
            .to_owned();
        write!(
            output,
            "[{}] {} {}\n{}\n\n",
            event.revision,
            kind,
            event.occurred_at,
            event.payload_json.as_str()
        )
        .expect("writing to a String cannot fail");
    }
    output
}

fn map_list_error(
    error: session_contract::SessionListInvocationError,
) -> PluginError<ExecuteError> {
    match error {
        session_contract::SessionListInvocationError::Domain(
            session_contract::ListError::InvalidCursor | session_contract::ListError::InvalidLimit,
        ) => PluginError::domain(ExecuteError::InvalidArguments),
        session_contract::SessionListInvocationError::Domain(
            session_contract::ListError::Unknown(unknown),
        ) => PluginError::domain(unknown_session_error(unknown.code)),
        session_contract::SessionListInvocationError::Runtime(error) => PluginError::runtime(error),
    }
}

fn map_read_error(
    error: session_contract::SessionReadInvocationError,
) -> PluginError<ExecuteError> {
    match error {
        session_contract::SessionReadInvocationError::Domain(
            session_contract::ReadError::InvalidCursor,
        ) => PluginError::domain(ExecuteError::InvalidArguments),
        session_contract::SessionReadInvocationError::Domain(
            session_contract::ReadError::NotFound,
        ) => PluginError::domain(ExecuteError::NotFound),
        session_contract::SessionReadInvocationError::Domain(
            session_contract::ReadError::Unknown(unknown),
        ) => PluginError::domain(unknown_session_error(unknown.code)),
        session_contract::SessionReadInvocationError::Runtime(error) => PluginError::runtime(error),
    }
}

fn unknown_session_error(code: String) -> ExecuteError {
    ExecuteError::ExecutionFailed {
        payload: command_contract::ExecutionFailedPayload {
            reason_code: code,
            message: "Session provider returned an unknown domain error".to_owned(),
            details_json: "{}".to_owned().try_into().expect("empty object is JSON"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_catalog_passes_the_generic_contract_validator() {
        command_contract::validate_catalog(&command_catalog()).unwrap();
    }

    #[test]
    fn limits_are_bounded_by_each_command() {
        assert_eq!(parse_limit(None, 20, 100).unwrap(), 20);
        assert_eq!(parse_limit(Some("100"), 20, 100).unwrap(), 100);
        assert!(parse_limit(Some("101"), 20, 100).is_err());
        assert!(parse_limit(Some("zero"), 20, 100).is_err());
    }

    #[test]
    fn revisions_use_the_session_uint64_wire_shape() {
        assert!(valid_uint64("0"));
        assert!(valid_uint64("18446744073709551615"));
        assert!(!valid_uint64("18446744073709551616"));
        assert!(!valid_uint64("-1"));
    }
}
