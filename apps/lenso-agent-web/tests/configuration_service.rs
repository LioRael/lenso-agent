use std::{
    net::{SocketAddr, TcpListener},
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};

use lenso_agent_host::{AgentHost, WebSurface};

const READ_TOKEN: &str = "service-smoke-read-token";
const WRITE_TOKEN: &str = "service-smoke-write-token";

struct ServiceProcess(Child);

impl Drop for ServiceProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn serves_one_durable_resource_with_read_and_write_scopes() {
    let root = tempfile::tempdir().unwrap();
    AgentHost::builder()
        .plugins(lenso_agent_default_plugins::link)
        .agent_home(root.path())
        .unwrap()
        .surface(WebSurface::browser())
        .build()
        .unwrap()
        .prepare_authoring()
        .unwrap();
    let address = available_address();
    let database = root.path().join("configuration.sqlite3");
    let mut command = Command::new(env!("CARGO_BIN_EXE_lenso-plugin-configuration-service"));
    command
        .args([
            "--listen",
            &address.to_string(),
            "--root",
            root.path().to_str().unwrap(),
            "--database",
            database.to_str().unwrap(),
            "--app",
            "agent",
            "--environment",
            "production",
        ])
        .env("LENSO_PLUGIN_CONFIGURATION_SERVICE_READ_TOKEN", READ_TOKEN)
        .env(
            "LENSO_PLUGIN_CONFIGURATION_SERVICE_WRITE_TOKEN",
            WRITE_TOKEN,
        )
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut service = ServiceProcess(command.spawn().unwrap());
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let resource = format!("http://{address}/v1/apps/agent/environments/production/plugins");
    let response = wait_for_service(&client, &resource, &mut service);
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body = response.json::<serde_json::Value>().unwrap();
    assert_eq!(body["schema"], "lenso.configuration.plugin-management.v1");

    let forbidden = client
        .post(format!(
            "http://{address}/v1/apps/agent/environments/production/plugins/lenso.agent.loop/agent/configuration/proposals"
        ))
        .bearer_auth(READ_TOKEN)
        .json(&serde_json::json!({
            "expectedRevision": body["revision"],
            "toml": "max_steps = 8\n",
        }))
        .send()
        .unwrap();
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);

    let hidden = client
        .get(format!(
            "http://{address}/v1/apps/other/environments/production/plugins"
        ))
        .bearer_auth(WRITE_TOKEN)
        .send()
        .unwrap();
    assert_eq!(hidden.status(), reqwest::StatusCode::NOT_FOUND);
    assert!(database.is_file());
}

fn available_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap()
}

fn wait_for_service(
    client: &reqwest::blocking::Client,
    resource: &str,
    service: &mut ServiceProcess,
) -> reqwest::blocking::Response {
    for _ in 0..40 {
        if let Some(status) = service.0.try_wait().unwrap() {
            let error = service
                .0
                .stderr
                .take()
                .map(|stderr| std::io::read_to_string(stderr).unwrap())
                .unwrap_or_default();
            panic!("configuration service exited with {status}: {error}");
        }
        if let Ok(response) = client.get(resource).bearer_auth(READ_TOKEN).send() {
            return response;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("configuration service did not become ready");
}
