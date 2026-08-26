use std::{env, fs, path::PathBuf, process::Command};

mod authority;
mod channel;
pub mod discord;
pub mod generation;
mod plugin_profiles;
pub mod plugins;
pub mod provenance;
pub mod telegram;
#[cfg(test)]
mod test_support;
pub mod tui;

/// Resolves the single product App Definition unless an exact Plan override is supplied.
pub fn default_plan() -> Result<PathBuf, String> {
    env::var_os("LENSO_RESOLVED_PLAN")
        .map(PathBuf::from)
        .map_or_else(resolve_base_plan, Ok)
}

fn resolve_base_plan() -> Result<PathBuf, String> {
    let definition = env::var_os("LENSO_APP_DEFINITION")
        .map_or_else(|| PathBuf::from("lenso.app.json"), PathBuf::from);
    let plan = PathBuf::from(".lenso").join("resolved-plan.json");
    let parent = plan
        .parent()
        .ok_or_else(|| format!("App Plan path `{}` has no parent", plan.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    let lenso = env::var_os("LENSO_BIN").unwrap_or_else(|| "lenso".into());
    let output = Command::new(&lenso)
        .args(["app", "resolve", "--definition"])
        .arg(&definition)
        .arg("--output")
        .arg(&plan)
        .output()
        .map_err(|error| {
            format!(
                "failed to run `{}`: {error}",
                PathBuf::from(&lenso).display()
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "failed to resolve the App from {}: {}",
            definition.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(plan)
}
