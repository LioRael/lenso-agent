use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema)]
#[schemars(deny_unknown_fields)]
struct ExecuteRequest {
    text: String,
}

#[derive(lenso::JsonSchema)]
#[schemars(deny_unknown_fields)]
struct ExecuteResponse {
    text: String,
}

#[derive(lenso::DomainError)]
enum OperationError {
    Unavailable,
}

#[lenso::capability(
    id = "external.task-board",
    major = 1,
    version = "1.0.0",
    portable = true,
    cross_lane_transfer = false
)]
trait Contract {
    async fn execute(
        &self,
        context: lenso::Ctx<'_>,
        request: ExecuteRequest,
    ) -> Result<ExecuteResponse, OperationError>;
}
