use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema)]
#[schemars(deny_unknown_fields)]
struct AuditRequest {}

#[derive(lenso::JsonSchema)]
#[schemars(deny_unknown_fields)]
struct AuditResponse {
    status: String,
}

#[derive(lenso::DomainError)]
enum OperationError {
    Unavailable,
}

#[lenso::capability(
    id = "external.task-board-audit",
    major = 1,
    version = "1.0.0",
    portable = true,
    cross_lane_transfer = false
)]
trait Contract {
    async fn audit(
        &self,
        context: lenso::Ctx<'_>,
        request: AuditRequest,
    ) -> Result<AuditResponse, OperationError>;
}
