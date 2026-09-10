use lenso_app_plan::{
    AppComposition, CapabilityBinding, CapabilityEndpointPlan, CapabilityRequirementPlan,
    PluginInstancePlan,
};
use lenso_auth_account_plugin::{AccountAuthConfig, AccountAuthOperator, assertion_public_key};
use lenso_capability_auth as auth;
use lenso_capability_auth_delegation as delegation;
use lenso_capability_credential_issuer as issuer;
use lenso_capability_identity_directory as directory;
use lenso_capability_secrets::{
    self as secrets, ResolveError, ResolveRequest, ResolveResponse, Secrets, SecretsEndpoint,
    SecretsProvider,
};
use lenso_kernel::{
    InvocationContext, Kernel, NativeApp, NativeRequestEndpoint, NativeRequestFuture,
    RuntimeFailure, ShutdownOutcome,
};
use lenso_native_adapter::{
    NativePluginFactory, NativePluginFactoryContext, NativePluginInstance, NativePluginRegistry,
};
use lenso_runner::TokioDriver;
use serde_json::{Value, json};

use std::{collections::BTreeMap, rc::Rc, time::Duration};
const CALLER_PACKAGE_ID: &str = "test.auth-caller";
const SECRETS_PACKAGE_ID: &str = "test.static-secrets";
const SIGNING_SECRET: &str = "integration-signing-secret-with-high-entropy";
const TOKEN_PEPPER: &str = "integration-token-pepper-with-high-entropy";
#[derive(Debug)]
struct CallerFactory;

impl NativePluginFactory for CallerFactory {
    fn package_id(&self) -> &'static str {
        CALLER_PACKAGE_ID
    }

    fn instantiate(
        &self,
        _context: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        Ok(NativePluginInstance::default())
    }
}

#[derive(Clone)]
struct StaticSecretsFactory {
    values: BTreeMap<String, String>,
}

impl std::fmt::Debug for StaticSecretsFactory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StaticSecretsFactory")
            .field("references", &self.values.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl NativePluginFactory for StaticSecretsFactory {
    fn package_id(&self) -> &'static str {
        SECRETS_PACKAGE_ID
    }

    fn instantiate(
        &self,
        _context: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        let endpoint = Rc::new(SecretsEndpoint::new(StaticSecretsProvider {
            values: self.values.clone(),
        })) as Rc<dyn NativeRequestEndpoint>;
        Ok(NativePluginInstance::new(vec![endpoint]))
    }
}

#[derive(Clone)]
struct StaticSecretsProvider {
    values: BTreeMap<String, String>,
}

impl std::fmt::Debug for StaticSecretsProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StaticSecretsProvider")
            .field("references", &self.values.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl SecretsProvider for StaticSecretsProvider {
    fn resolve(
        &self,
        _context: InvocationContext,
        request: ResolveRequest,
    ) -> NativeRequestFuture<Secrets> {
        let result = self
            .values
            .get(&request.reference)
            .cloned()
            .map(|value| ResolveResponse { value })
            .ok_or(ResolveError::UnknownReference);
        Box::pin(std::future::ready(Ok(result)))
    }
}

use lenso_capability_http_endpoint as http;
fn instance(key: &str, descriptor: &str, config: &Value) -> PluginInstancePlan {
    let descriptor: Value = serde_json::from_str(descriptor).unwrap();
    let mut result = PluginInstancePlan::new(key, descriptor["plugin_id"].as_str().unwrap())
        .with_configuration(config.to_string());
    for entry in descriptor["provided_capabilities"].as_array().unwrap() {
        let mut endpoint = CapabilityEndpointPlan::new(
            entry["capability_id"].as_str().unwrap(),
            entry["descriptor_version"].as_str().unwrap(),
            entry["operations"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap()),
        );
        if entry["cross_lane_transfer"] == true {
            endpoint = endpoint.with_cross_lane_transfer();
        }
        result = result.with_capability(endpoint);
    }
    for entry in descriptor["required_capabilities"].as_array().unwrap() {
        result = result.with_requirement(CapabilityRequirementPlan::one(
            entry["capability_id"].as_str().unwrap(),
            entry["descriptor_version"].as_str().unwrap(),
        ));
    }
    result
}

use lenso_capability_access_control_admin as acl_admin;
use lenso_capability_organization_admin as org_admin;
use lenso_capability_organization_membership_admin as org_members;
use lenso_capability_password_auth as password;
use lenso_capability_projects as projects;
use lenso_capability_projects_admin as project_admin;
const ORIGIN: &str = "http://127.0.0.1:55440";
const ISSUER: &str = "acceptance.projects";
const TEST_PASSWORD: &str = "Local-acceptance-only-2026!";
fn organization(config: Value) -> PluginInstancePlan {
    let mut plan = PluginInstancePlan::new("organization", "lenso.organization.postgres")
        .with_configuration(config.to_string())
        .with_requirement(CapabilityRequirementPlan::one(
            secrets::CAPABILITY_ID,
            secrets::DESCRIPTOR_VERSION,
        ));
    for source in [
        include_str!("../organization-admin.json"),
        include_str!("../organization-directory.json"),
        include_str!("../organization-membership.json"),
        include_str!("../organization-membership-admin.json"),
    ] {
        let d: Value = serde_json::from_str(source).unwrap();
        let mut ops: Vec<_> = d["operations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["name"].as_str().unwrap())
            .collect();
        ops.sort();
        plan = plan.with_capability(
            CapabilityEndpointPlan::new(
                d["id"].as_str().unwrap(),
                d["version"].as_str().unwrap(),
                ops,
            )
            .with_cross_lane_transfer(),
        );
    }
    plan
}
async fn start(url: &str, prefix: &str) -> NativeApp {
    use lenso_access_control_postgres_plugin::AccessControlOperator;
    use lenso_auth_password_plugin::PasswordAuthOperator;
    use lenso_organization_postgres_plugin::OrganizationOperator;
    use lenso_projects_postgres_plugin::ProjectsOperator;
    AccountAuthOperator::setup(url, &format!("{prefix}_account"))
        .await
        .unwrap();
    PasswordAuthOperator::setup(url, &format!("{prefix}_password"))
        .await
        .unwrap();
    OrganizationOperator::setup(url, &format!("{prefix}_organization"))
        .await
        .unwrap();
    AccessControlOperator::setup(url, &format!("{prefix}_acl"))
        .await
        .unwrap();
    ProjectsOperator::setup(url, &format!("{prefix}_projects"))
        .await
        .unwrap();
    let public = assertion_public_key(SIGNING_SECRET);
    let account = AccountAuthConfig::new(
        format!("{prefix}_account"),
        ISSUER,
        &public,
        "auth/database-url",
        "auth/assertion-signing-key",
        "auth/token-pepper",
        60,
    )
    .unwrap()
    .with_delegation_callers(vec!["consent".into()])
    .unwrap();
    let mut instances = vec![
        instance(
            "account",
            lenso_auth_account_plugin::PLUGIN_DESCRIPTOR_JSON,
            &serde_json::to_value(account).unwrap(),
        ),
        instance(
            "password",
            lenso_auth_password_plugin::PLUGIN_DESCRIPTOR_JSON,
            &json!({"schema":format!("{prefix}_password"),"database_url_secret":"auth/database-url","audience":["lenso.agent.tool-provider@2:catalog", "lenso.agent.tool-provider@2:execute", "lenso.projects@1:list_issue_workflow_states", "lenso.projects@1:create_project", "lenso.projects@1:get_project", "lenso.projects@1:list_projects", "lenso.projects@1:update_project", "lenso.projects@1:archive_project", "lenso.projects@1:create_issue", "lenso.projects@1:get_issue", "lenso.projects@1:list_issues", "lenso.projects@1:update_issue", "lenso.projects@1:move_issue", "lenso.projects@1:archive_issue", "lenso.projects@1:put_external_link", "lenso.projects@1:list_activity", "lenso.projects-admin@1:put_team", "lenso.projects-admin@1:list_teams", "lenso.projects-admin@1:set_team_member", "lenso.projects-admin@1:put_workflow_state", "lenso.projects-admin@1:get_workflow_state", "lenso.projects-admin@1:reorder_workflow_states", "lenso.projects-admin@1:archive_workflow_state", "lenso.projects-admin@1:delete_workflow_state", "lenso.projects-admin@1:list_workflow_states", "lenso.projects-admin@1:put_project_status", "lenso.projects-admin@1:get_project_status", "lenso.projects-admin@1:reorder_project_statuses", "lenso.projects-admin@1:archive_project_status", "lenso.projects-admin@1:delete_project_status", "lenso.projects-admin@1:list_project_statuses", "lenso.projects-admin@1:put_label", "lenso.projects-admin@1:list_labels", "lenso.projects-admin@1:put_cycle", "lenso.projects-admin@1:list_cycles", "lenso.projects-admin@1:put_milestone", "lenso.projects-admin@1:list_milestones", "lenso.projects-collaboration@1:add_comment", "lenso.projects-collaboration@1:update_comment", "lenso.projects-collaboration@1:delete_comment", "lenso.projects-collaboration@1:list_comments", "lenso.projects-collaboration@1:create_project_update", "lenso.projects-collaboration@1:list_project_updates", "lenso.projects-collaboration@1:add_issue_relation", "lenso.projects-collaboration@1:remove_issue_relation", "lenso.access-control-admin@1:bootstrap_scope", "lenso.access-control-admin@1:create_role", "lenso.access-control-admin@1:set_role_permissions", "lenso.access-control-admin@1:delete_role", "lenso.access-control-admin@1:assign_role", "lenso.access-control-admin@1:revoke_role"],"session_ttl_seconds":7200,"max_failures":5,"failure_window_seconds":60}),
        ),
        instance(
            "consent",
            lenso_auth_agent_connection_plugin::PLUGIN_DESCRIPTOR_JSON,
            &json!({"origin":ORIGIN,"label":"Projects acceptance","login_path":"/login","audience":["lenso.agent.tool-provider@2:catalog", "lenso.agent.tool-provider@2:execute", "lenso.projects@1:get_issue", "lenso.projects@1:list_issues", "lenso.projects@1:list_projects", "lenso.projects@1:list_issue_workflow_states", "lenso.projects@1:update_issue", "lenso.projects@1:create_project", "lenso.projects@1:get_project", "lenso.projects@1:list_activity", "lenso.projects-admin@1:list_teams", "lenso.projects-admin@1:list_project_statuses", "lenso.projects-admin@1:list_workflow_states"],"grant_ttl_seconds":3600}),
        ),
        organization(
            json!({"schema":format!("{prefix}_organization"),"database_url_secret":"auth/database-url","admin_callers":["caller"],"directory_callers":["caller"],"membership_admin_callers":["caller"]}),
        ),
        instance(
            "acl",
            lenso_access_control_postgres_plugin::PLUGIN_DESCRIPTOR_JSON,
            &json!({"schema":format!("{prefix}_acl"),"database_url_secret":"auth/database-url","auth_issuer":ISSUER,"auth_assertion_public_key":public,"bootstrap_callers":["caller"],"directory_callers":["caller"]}),
        ),
        instance(
            "projects",
            lenso_projects_postgres_plugin::PLUGIN_DESCRIPTOR_JSON,
            &json!({"schema":format!("{prefix}_projects"),"database_url_secret":"auth/database-url","auth_issuer":ISSUER,"auth_assertion_public_key":public,"project_callers":["caller","tools","projects-web"],"admin_callers":["caller","projects-web"],"governance_callers":["caller"]}),
        ),
        instance(
            "tools",
            lenso_projects_agent_tools_plugin::PLUGIN_DESCRIPTOR_JSON,
            &json!({}),
        ),
        instance(
            "ingress",
            lenso_projects_agent_web_plugin::PLUGIN_DESCRIPTOR_JSON,
            &json!({}),
        ),
        instance(
            "projects-web",
            lenso_projects_web_plugin::PLUGIN_DESCRIPTOR_JSON,
            &json!({"origin":ORIGIN}),
        ),
        PluginInstancePlan::new("secrets", SECRETS_PACKAGE_ID).with_capability(
            CapabilityEndpointPlan::new(
                secrets::CAPABILITY_ID,
                secrets::DESCRIPTOR_VERSION,
                ["resolve"],
            ),
        ),
    ];
    // Fixture composition only: concrete production Plugins provide every business operation.
    let mut caller = PluginInstancePlan::new("caller", CALLER_PACKAGE_ID);
    let mut web = PluginInstancePlan::new("web-caller", CALLER_PACKAGE_ID).with_requirement(
        CapabilityRequirementPlan::one(http::CAPABILITY_ID, http::DESCRIPTOR_VERSION),
    );
    let _ = &mut web;
    let mut bindings = vec![CapabilityBinding::new(
        "web-caller",
        http::CAPABILITY_ID,
        http::DESCRIPTOR_VERSION,
        "ingress",
    )];
    for (id, version, target) in [
        (auth::CAPABILITY_ID, auth::DESCRIPTOR_VERSION, "account"),
        (
            password::CAPABILITY_ID,
            password::DESCRIPTOR_VERSION,
            "password",
        ),
        (issuer::CAPABILITY_ID, issuer::DESCRIPTOR_VERSION, "account"),
        (http::CAPABILITY_ID, http::DESCRIPTOR_VERSION, "consent"),
        (
            org_admin::CAPABILITY_ID,
            org_admin::DESCRIPTOR_VERSION,
            "organization",
        ),
        (
            org_members::CAPABILITY_ID,
            org_members::DESCRIPTOR_VERSION,
            "organization",
        ),
        (
            acl_admin::CAPABILITY_ID,
            acl_admin::DESCRIPTOR_VERSION,
            "acl",
        ),
        (
            projects::CAPABILITY_ID,
            projects::DESCRIPTOR_VERSION,
            "projects",
        ),
        (
            project_admin::CAPABILITY_ID,
            project_admin::DESCRIPTOR_VERSION,
            "projects",
        ),
    ] {
        caller = caller.with_requirement(CapabilityRequirementPlan::one(id, version));
        bindings.push(CapabilityBinding::new("caller", id, version, target));
    }
    // Derive exact bindings from each Plugin's declared requirement; never fabricate endpoints.
    for (key, source) in [
        ("account", lenso_auth_account_plugin::PLUGIN_DESCRIPTOR_JSON),
        (
            "projects-web",
            lenso_projects_web_plugin::PLUGIN_DESCRIPTOR_JSON,
        ),
        (
            "password",
            lenso_auth_password_plugin::PLUGIN_DESCRIPTOR_JSON,
        ),
        (
            "consent",
            lenso_auth_agent_connection_plugin::PLUGIN_DESCRIPTOR_JSON,
        ),
        (
            "acl",
            lenso_access_control_postgres_plugin::PLUGIN_DESCRIPTOR_JSON,
        ),
        (
            "projects",
            lenso_projects_postgres_plugin::PLUGIN_DESCRIPTOR_JSON,
        ),
        (
            "tools",
            lenso_projects_agent_tools_plugin::PLUGIN_DESCRIPTOR_JSON,
        ),
        (
            "ingress",
            lenso_projects_agent_web_plugin::PLUGIN_DESCRIPTOR_JSON,
        ),
    ] {
        let d: Value = serde_json::from_str(source).unwrap();
        for req in d["required_capabilities"].as_array().unwrap() {
            let id = req["capability_id"].as_str().unwrap();
            let ver = req["descriptor_version"].as_str().unwrap();
            let target = match id {
                secrets::CAPABILITY_ID => "secrets",
                auth::CAPABILITY_ID
                | directory::CAPABILITY_ID
                | issuer::CAPABILITY_ID
                | delegation::CAPABILITY_ID => "account",
                lenso_capability_organization_membership::CAPABILITY_ID => "organization",
                lenso_capability_access_control::CAPABILITY_ID => "acl",
                projects::CAPABILITY_ID
                | lenso_capability_projects_collaboration::CAPABILITY_ID
                | project_admin::CAPABILITY_ID => "projects",
                lenso_capability_agent_tool_provider::CAPABILITY_ID => "tools",
                _ => panic!("unexpected fixture binding {id}"),
            };
            bindings.push(CapabilityBinding::new(key, id, ver, target));
        }
    }
    bindings.push(CapabilityBinding::new(
        "organization",
        secrets::CAPABILITY_ID,
        secrets::DESCRIPTOR_VERSION,
        "secrets",
    ));
    let projects_web_caller =
        PluginInstancePlan::new("projects-web-caller", CALLER_PACKAGE_ID).with_requirement(
            CapabilityRequirementPlan::one(http::CAPABILITY_ID, http::DESCRIPTOR_VERSION),
        );
    bindings.push(CapabilityBinding::new(
        "projects-web-caller",
        http::CAPABILITY_ID,
        http::DESCRIPTOR_VERSION,
        "projects-web",
    ));
    instances.extend([caller, web, projects_web_caller]);
    Kernel::start_native(
        AppComposition::new(instances, bindings).resolve().unwrap(),
        TokioDriver::new(),
        NativePluginRegistry::new()
            .with_linked_factories()
            .with_factory(CallerFactory)
            .with_factory(lenso_organization_postgres_plugin::OrganizationFactory)
            .with_factory(StaticSecretsFactory {
                values: BTreeMap::from([
                    ("auth/database-url".into(), url.into()),
                    ("auth/assertion-signing-key".into(), SIGNING_SECRET.into()),
                    ("auth/token-pepper".into(), TOKEN_PEPPER.into()),
                ]),
            }),
    )
    .await
    .unwrap()
}
async fn actor(app: &NativeApp, credential: &str) -> InvocationContext {
    let response = app
        .invoke::<auth::Auth>(
            "caller",
            "authenticate",
            auth::AuthRequest {
                credential: Some(auth::AuthenticateRequestCredential {
                    scheme: "session".into(),
                    value: credential.into(),
                }),
            },
        )
        .await
        .unwrap()
        .unwrap();
    let lenso_auth_sdk::AuthOutcome::Authenticated(assertion) =
        lenso_auth_sdk::decode_auth_response(response).unwrap()
    else {
        panic!("missing actor")
    };
    assertion
        .attach(app.invocation_context_after(
            Duration::from_secs(30),
            lenso_kernel::CancellationToken::new(),
        ))
        .unwrap()
}
macro_rules! invoke {
    ($app:expr,$marker:ty,$operation:expr,$value:expr) => {
        $app.invoke::<$marker>(
            "caller",
            $operation,
            serde_json::from_value($value).unwrap(),
        )
        .await
        .unwrap()
        .unwrap()
    };
}
macro_rules! authorized {
    ($app:expr,$context:expr,$marker:ty,$operation:expr,$value:expr) => {
        $app.handle::<$marker>("caller")
            .unwrap()
            .invoke_with_context(
                $operation,
                $context.clone(),
                serde_json::from_value($value).unwrap(),
            )
            .await
            .unwrap()
            .unwrap()
    };
}
async fn seed(app: &NativeApp) -> Value {
    let alice = invoke!(
        app,
        password::PasswordRegister,
        "register",
        json!({"identifier":"alice@example.test","password":TEST_PASSWORD})
    );
    let bob = invoke!(
        app,
        password::PasswordRegister,
        "register",
        json!({"identifier":"bob@example.test","password":TEST_PASSWORD})
    );
    let org=invoke!(app,org_admin::OrganizationAdminCreateOrganization,"create_organization",json!({"idempotency_key":"organization","name":"Acceptance","owner_subject":alice.subject,"slug":"acceptance"})).organization_id;
    invoke!(
        app,
        org_members::OrganizationMembershipAdminAddMember,
        "add_member",
        json!({"idempotency_key":"bob","organization_id":org,"subject":bob.subject})
    );
    let other_org=invoke!(app,org_admin::OrganizationAdminCreateOrganization,"create_organization",json!({"idempotency_key":"other-organization","name":"Other organization","owner_subject":bob.subject,"slug":"other-organization"})).organization_id;
    let scope = json!({"kind":"organization","id":org});
    invoke!(
        app,
        acl_admin::AccessControlAdminBootstrapScope,
        "bootstrap_scope",
        json!({"scope":scope,"subject":alice.subject})
    );
    let ctx = actor(app, &alice.credential).await;
    authorized!(
        app,
        ctx,
        acl_admin::AccessControlAdminCreateRole,
        "create_role",
        json!({"scope":scope,"role_id":"editor","name":"Editor"})
    );
    authorized!(
        app,
        ctx,
        acl_admin::AccessControlAdminSetRolePermissions,
        "set_role_permissions",
        json!({"scope":scope,"role_id":"editor","permissions":["projects.read","projects.write","projects.admin"]})
    );
    for subject in [&alice.subject, &bob.subject] {
        authorized!(
            app,
            ctx,
            acl_admin::AccessControlAdminAssignRole,
            "assign_role",
            json!({"scope":scope,"role_id":"editor","subject":subject})
        );
    }
    for (team, private) in [("public", false), ("private", true)] {
        authorized!(
            app,
            ctx,
            project_admin::ProjectsAdminPutTeam,
            "put_team",
            json!({"idempotency_key":format!("team-{team}"),"organization_id":org,"team_id":team,"key":team.to_uppercase(),"name":team,"description":null,"private":private,"default_workflow_state_id":null,"expected_revision":null})
        );
        authorized!(
            app,
            ctx,
            project_admin::ProjectsAdminSetTeamMember,
            "set_team_member",
            json!({"idempotency_key":format!("member-{team}"),"organization_id":org,"team_id":team,"subject":alice.subject,"active":true})
        );
        authorized!(
            app,
            ctx,
            project_admin::ProjectsAdminPutWorkflowState,
            "put_workflow_state",
            json!({"idempotency_key":format!("state-{team}"),"organization_id":org,"state_id":format!("started-{team}"),"team_id":team,"name":"In progress","category":"started","color":"#3366FF","position":1,"expected_revision":null})
        );
        authorized!(
            app,
            ctx,
            projects::ProjectsCreateProject,
            "create_project",
            json!({"idempotency_key":format!("project-{team}"),"organization_id":org,"project_id":format!("project-{team}"),"name":format!("{team} project"),"summary":null,"lead_team_id":team,"team_ids":[team],"status_id":null,"milestone_id":null,"start_date":null,"target_date":null})
        );
        authorized!(
            app,
            ctx,
            projects::ProjectsCreateIssue,
            "create_issue",
            json!({"idempotency_key":format!("issue-{team}"),"organization_id":org,"issue_id":format!("issue-{team}"),"project_id":format!("project-{team}"),"team_id":team,"title":format!("{team} acceptance issue"),"description":null,"priority":"medium","workflow_state_id":format!("started-{team}"),"cycle_id":null,"milestone_id":null,"parent_issue_id":null,"label_ids":[]})
        );
    }
    json!({"origin":ORIGIN,"organization_id":org,"other_organization_id":other_org,"alice_subject":alice.subject,"bob_subject":bob.subject,"public_issue_id":"issue-public","private_issue_id":"issue-private"})
}
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let url = std::env::var("LENSO_POSTGRES_TEST_URL").expect("dedicated test PostgreSQL required");
    if let Ok(path) = std::env::var("LENSO_ACCEPTANCE_RECEIPT") {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("cannot remove stale acceptance receipt: {error}"),
        }
    }
    let prefix = format!("acceptance_{}", std::process::id());
    tokio::task::LocalSet::new()
        .run_until(async {
            let app = start(&url, &prefix).await;
            let receipt = seed(&app).await;
            serve(&app, &receipt).await;
            assert_eq!(
                app.shutdown(Duration::from_secs(3)).await,
                ShutdownOutcome::Clean
            );
            use sqlx::Executor;
            let pool = sqlx::PgPool::connect(&url).await.unwrap();
            for suffix in ["projects", "acl", "organization", "password", "account"] {
                pool.execute(sqlx::AssertSqlSafe(format!(
                    "DROP SCHEMA \"{prefix}_{suffix}\" CASCADE"
                )))
                .await
                .unwrap();
            }
            pool.close().await;
        })
        .await;
}
struct Incoming {
    method: String,
    path: String,
    query: Option<String>,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
    reply: tokio::sync::oneshot::Sender<axum::response::Response>,
}
fn reply(status: u16, body: impl Into<String>) -> axum::response::Response {
    axum::http::Response::builder()
        .status(status)
        .header("content-type", "text/html; charset=utf-8")
        .header("cache-control", "no-store")
        .header("referrer-policy", "same-origin")
        .header("x-content-type-options", "nosniff")
        .body(axum::body::Body::from(body.into()))
        .unwrap()
}
fn selected(headers: &axum::http::HeaderMap) -> Result<Option<String>, ()> {
    let bearer = headers
        .get("authorization")
        .and_then(|x| x.to_str().ok())
        .and_then(|x| x.strip_prefix("Bearer "));
    let cookie = headers
        .get("cookie")
        .and_then(|x| x.to_str().ok())
        .and_then(|x| {
            x.split(';')
                .find_map(|c| c.trim().strip_prefix("acceptance_session="))
        });
    match (bearer, cookie) {
        (Some(_), Some(_)) => Err(()),
        (Some(x), None) | (None, Some(x)) => Ok(Some(x.into())),
        _ => Ok(None),
    }
}
async fn dispatch(
    app: &NativeApp,
    request: &Incoming,
    receipt: &Value,
) -> axum::response::Response {
    let Ok(credential) = selected(&request.headers) else {
        return reply(400, "Ambiguous credentials");
    };
    if request.path == "/login" && request.method == "GET" {
        let fields: BTreeMap<String, String> =
            serde_urlencoded::from_str(request.query.as_deref().unwrap_or("")).unwrap_or_default();
        let return_to = fields.get("return_to").map(String::as_str).unwrap_or("");
        let return_to = return_to
            .replace('&', "&amp;")
            .replace('"', "&quot;")
            .replace('<', "&lt;");
        return reply(
            200,
            format!(
                "<!doctype html><html lang=en><meta charset=utf-8><title>Projects acceptance login</title><main><h1>Projects acceptance</h1><form method=post action=/login><input type=hidden name=return_to value=\"{return_to}\"><label>Email <input name=identifier type=email required></label><label>Password <input name=password type=password required></label><button>Sign in</button></form></main></html>"
            ),
        );
    }
    if request.path == "/login" && request.method == "POST" {
        if request.headers.get("origin").and_then(|x| x.to_str().ok()) != Some(ORIGIN) {
            return reply(403, "Origin rejected");
        }
        let Ok(fields) = serde_urlencoded::from_bytes::<BTreeMap<String, String>>(&request.body)
        else {
            return reply(400, "Invalid form");
        };
        let (Some(identifier), Some(password)) = (fields.get("identifier"), fields.get("password"))
        else {
            return reply(400, "Missing login fields");
        };
        let result = app
            .invoke::<password::PasswordLogin>(
                "caller",
                "login",
                password::LoginRequest {
                    identifier: identifier.clone(),
                    password: password.clone(),
                },
            )
            .await;
        let Ok(Ok(login)) = result else {
            return reply(401, "Sign in failed");
        };
        let mut response = reply(
            200,
            "<h1>Signed in</h1><p>Return to Agent and connect Projects, then approve this connection in the browser.</p><a href=/>Projects</a>",
        );
        response.headers_mut().insert(
            "set-cookie",
            format!(
                "acceptance_session={}; HttpOnly; SameSite=Strict; Path=/; Max-Age=7200",
                login.credential
            )
            .parse()
            .unwrap(),
        );
        if let Some(path) = fields.get("return_to").filter(|path| {
            (path.starts_with("/auth/agent/authorize?") || path.starts_with("/projects?"))
                && !path.contains(['\\', '\r', '\n'])
        }) {
            *response.status_mut() = axum::http::StatusCode::SEE_OTHER;
            if let Ok(location) = path.parse() {
                response.headers_mut().insert("location", location);
            }
        }
        return response;
    }
    if request.path == "/" {
        return reply(
            200,
            format!(
                "<h1>Projects acceptance</h1><p>Organization: {}</p><p>Public issue: issue-public. Private issue: issue-private.</p><a href=/login>Sign in / switch account</a><form method=post action=/logout><button>Revoke browser session and delegated grants</button></form>",
                receipt["organization_id"].as_str().unwrap()
            ),
        );
    }
    if request.path == "/logout" && request.method == "POST" {
        if request.headers.get("origin").and_then(|x| x.to_str().ok()) != Some(ORIGIN) {
            return reply(403, "Origin rejected");
        }
        let Some(credential) = credential else {
            return reply(401, "Sign in required");
        };
        if !matches!(
            app.invoke::<issuer::CredentialIssuerRevokeCredential>(
                "caller",
                "revoke_credential",
                issuer::RevokeCredentialRequest {
                    scheme: "session".into(),
                    credential
                }
            )
            .await,
            Ok(Ok(_))
        ) {
            return reply(401, "Revocation failed");
        }
        let mut response = reply(
            200,
            "<h1>Session revoked</h1><p>Delegated Agent access from this session is no longer valid.</p>",
        );
        response.headers_mut().insert(
            "set-cookie",
            "acceptance_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"
                .parse()
                .unwrap(),
        );
        return response;
    }
    let (caller, route) = match (request.method.as_str(), request.path.as_str()) {
        ("POST", "/auth/agent/connection/begin") => ("caller", "auth.agent-connection.begin"),
        ("POST", "/auth/agent/connection/poll") => ("caller", "auth.agent-connection.poll"),
        ("GET", "/auth/agent/authorize") => ("caller", "auth.agent-connection.authorize"),
        ("POST", "/auth/agent/approve") => ("caller", "auth.agent-connection.approve"),
        ("POST", "/api/projects") => ("projects-web-caller", "projects.web.projects.create"),
        ("GET", path) if path.starts_with("/api/projects/") && path.ends_with("/issues") => {
            ("projects-web-caller", "projects.web.issues.list")
        }
        ("GET", path)
            if path.starts_with("/api/projects/")
                && !path.starts_with("/api/projects/catalog/") =>
        {
            ("projects-web-caller", "projects.web.projects.detail")
        }
        ("GET", "/api/projects") => ("projects-web-caller", "projects.web.projects.list"),
        ("GET", "/api/projects/catalog/teams") => {
            ("projects-web-caller", "projects.web.catalog.teams")
        }
        ("GET", "/api/projects/catalog/project-statuses") => (
            "projects-web-caller",
            "projects.web.catalog.project-statuses",
        ),
        ("GET", "/api/projects/catalog/workflow-states") => (
            "projects-web-caller",
            "projects.web.catalog.workflow-states",
        ),
        ("GET", "/api/projects/catalog/cycles") => {
            ("projects-web-caller", "projects.web.catalog.cycles")
        }
        ("GET", "/projects") => ("projects-web-caller", "projects.web.page"),
        ("GET", "/projects/assets/app.css") => ("projects-web-caller", "projects.web.css"),
        ("GET", "/projects/assets/app.js") => ("projects-web-caller", "projects.web.js"),
        ("GET", path) if path.starts_with("/api/issues/") && path.ends_with("/activity") => {
            ("projects-web-caller", "projects.web.issues.activity")
        }
        ("GET", path) if path.starts_with("/api/issues/") => {
            ("projects-web-caller", "projects.web.issues.detail")
        }
        ("GET", "/projects/agent/tools") => ("web-caller", "projects.agent.catalog"),
        ("GET", "/projects/agent/manifest") => ("web-caller", "projects.agent.manifest"),
        ("POST", "/projects/agent/tools/execute") => ("web-caller", "projects.agent.execute"),
        _ => return reply(404, "Not found"),
    };
    let response = app
        .invoke::<http::EndpointHandle>(
            caller,
            "handle",
            http::HandleRequest {
                method: request.method.clone(),
                path: request.path.clone(),
                route_id: route.into(),
                request_id: uuid::Uuid::new_v4().to_string(),
                query: request.query.clone(),
                body: request.body.clone().into(),
                credential: credential.map(|value| http::HandleRequestCredential {
                    scheme: "session".into(),
                    value,
                }),
                headers: ["origin", "content-type"]
                    .into_iter()
                    .filter_map(|name| {
                        request
                            .headers
                            .get(name)
                            .and_then(|v| v.to_str().ok())
                            .map(|value| http::HandleRequestHeadersItem {
                                name: name.into(),
                                value: value.into(),
                            })
                    })
                    .collect(),
                path_parameters: if matches!(
                    route,
                    "projects.web.projects.detail" | "projects.web.issues.list"
                ) {
                    vec![http::HandleRequestPathParametersItem {
                        name: "project_id".into(),
                        value: request
                            .path
                            .trim_start_matches("/api/projects/")
                            .trim_end_matches("/issues")
                            .into(),
                    }]
                } else if route == "projects.web.issues.activity" {
                    vec![http::HandleRequestPathParametersItem {
                        name: "issue_id".into(),
                        value: request
                            .path
                            .trim_start_matches("/api/issues/")
                            .trim_end_matches("/activity")
                            .into(),
                    }]
                } else if route == "projects.web.issues.detail" {
                    vec![http::HandleRequestPathParametersItem {
                        name: "issue_ref".into(),
                        value: request.path.trim_start_matches("/api/issues/").into(),
                    }]
                } else {
                    vec![]
                },
            },
        )
        .await;
    let Ok(Ok(response)) = response else {
        return reply(503, "Business App unavailable");
    };
    let mut builder = axum::http::Response::builder().status(response.status as u16);
    for header in response.headers {
        builder = builder.header(header.name, header.value);
    }
    builder
        .body(axum::body::Body::from(response.body.as_ref().to_vec()))
        .unwrap()
}
async fn serve(app: &NativeApp, receipt: &Value) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Incoming>(32);
    let router = axum::Router::new().fallback(move |request: axum::extract::Request| {
        let tx = tx.clone();
        async move {
            let (parts, body) = request.into_parts();
            let Ok(body) = axum::body::to_bytes(body, 300_000).await else {
                return reply(413, "Request too large");
            };
            let (send, receive) = tokio::sync::oneshot::channel();
            if tx
                .send(Incoming {
                    method: parts.method.to_string(),
                    path: parts.uri.path().into(),
                    query: parts.uri.query().map(str::to_owned),
                    headers: parts.headers,
                    body: body.to_vec(),
                    reply: send,
                })
                .await
                .is_err()
            {
                return reply(503, "Stopped");
            }
            receive.await.unwrap_or_else(|_| reply(503, "Stopped"))
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:55440")
        .await
        .unwrap();
    println!("{}", receipt);
    if let Ok(path) = std::env::var("LENSO_ACCEPTANCE_RECEIPT") {
        std::fs::write(path, serde_json::to_vec_pretty(receipt).unwrap()).unwrap();
    }
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    loop {
        tokio::select! {_=tokio::signal::ctrl_c()=>break,Some(request)=rx.recv()=>{let response=dispatch(app,&request,receipt).await;let _=request.reply.send(response);}}
    }
    server.abort();
    let _ = server.await;
}
