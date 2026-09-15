//! Resolve “assign to me” from the Turn-pinned business grant.
use super::tools;
use serde_json::{Value, json};
const ASSIGN_SELF: &str = "projects_assign_issue_to_me";
const SET_ASSIGNEE: &str = "projects_set_issue_assignee";

pub(super) fn extend_catalog(catalog: &mut tools::CatalogResponse) {
    if catalog.tools.iter().any(|tool| tool.name == ASSIGN_SELF) {
        return;
    }
    let Some(mut tool) = catalog
        .tools
        .iter()
        .find(|tool| tool.name == SET_ASSIGNEE)
        .cloned()
    else {
        return;
    };
    let Ok(mut schema) = serde_json::from_str::<Value>(tool.input_schema_json.as_str()) else {
        return;
    };
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        properties.remove("assignee_subject");
    }
    if let Some(required) = schema.get_mut("required").and_then(Value::as_array_mut) {
        required.retain(|name| name != "assignee_subject");
    }
    tool.name = ASSIGN_SELF.into();
    tool.description = "Assign this Issue to the user connected to this Projects App. Use this for 'assign to me'; do not guess a subject ID. Supply the latest Issue revision and a stable idempotency key. Other Issue fields remain unchanged.".into();
    tool.input_schema_json = schema.to_string().parse().expect("serialized schema");
    catalog.tools.push(tool);
}

pub(super) fn resolve(
    mut request: tools::ExecuteRequest,
    subject: Option<&str>,
) -> Result<tools::ExecuteRequest, tools::ExecuteError> {
    if request.name != ASSIGN_SELF {
        return Ok(request);
    }
    let subject = subject
        .filter(|subject| !subject.is_empty())
        .ok_or(tools::ExecuteError::PermissionDenied)?;
    let mut arguments: Value = serde_json::from_str(request.arguments_json.as_str())
        .map_err(|_| tools::ExecuteError::InvalidArguments)?;
    let object = arguments
        .as_object_mut()
        .ok_or(tools::ExecuteError::InvalidArguments)?;
    if object.keys().any(|key| {
        ![
            "organization_id",
            "issue_id",
            "expected_revision",
            "idempotency_key",
        ]
        .contains(&key.as_str())
    }) {
        return Err(tools::ExecuteError::InvalidArguments);
    }
    object.insert("assignee_subject".into(), json!(subject));
    request.name = SET_ASSIGNEE.into();
    request.arguments_json = arguments.to_string().parse().expect("serialized arguments");
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(value: &Value) -> tools::ExecuteRequest {
        tools::ExecuteRequest {
            name: ASSIGN_SELF.into(),
            arguments_json: value.to_string().parse().unwrap(),
        }
    }
    #[test]
    fn self_assignment_uses_bound_subject_and_rejects_identity_overrides() {
        let value = json!({"organization_id":"org", "issue_id":"issue", "expected_revision":"3", "idempotency_key":"retry"});
        let result = resolve(request(&value), Some("bound-user")).unwrap();
        assert_eq!(result.name, SET_ASSIGNEE);
        let args: Value = serde_json::from_str(result.arguments_json.as_str()).unwrap();
        assert_eq!(args["assignee_subject"], "bound-user");
        assert_eq!(args["idempotency_key"], "retry");
        assert!(resolve(request(&value), None).is_err());
        assert!(
            resolve(
                request(&json!({"assignee_subject":"other"})),
                Some("bound-user")
            )
            .is_err()
        );
        assert!(resolve(request(&json!({"actor":"other"})), Some("bound-user")).is_err());
    }
    #[test]
    fn self_tool_requires_remote_assignment_support_and_preserves_execution_class() {
        let mut catalog = tools::CatalogResponse { tools: vec![] };
        extend_catalog(&mut catalog);
        assert!(catalog.tools.is_empty());
        catalog.tools.push(tools::ToolDefinition {
            name: SET_ASSIGNEE.into(), description: "assign".into(),
            execution: tools::ToolExecutionClass::Exclusive,
            input_schema_json: json!({"type":"object","additionalProperties":false,"required":["issue_id","assignee_subject"],"properties":{"issue_id":{"type":"string"},"assignee_subject":{"type":["string","null"]}}}).to_string().parse().unwrap(),
        });
        extend_catalog(&mut catalog);
        extend_catalog(&mut catalog);
        assert_eq!(catalog.tools.len(), 2);
        let tool = &catalog.tools[1];
        assert_eq!(tool.execution, tools::ToolExecutionClass::Exclusive);
        let schema: Value = serde_json::from_str(tool.input_schema_json.as_str()).unwrap();
        assert_eq!(schema["required"], json!(["issue_id"]));
        assert!(schema["properties"].get("assignee_subject").is_none());
    }
}
