//! Host-admitted interactive history namespace. Native baggage is not a grant;
//! ingress and Session authentication retain final authorization.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionNamespace {
    pub consumer: String,
    pub user: String,
    pub project: String,
}
impl SessionNamespace {
    pub fn validate(&self) -> Result<(), String> {
        if [&self.consumer, &self.user, &self.project]
            .iter()
            .any(|s| s.is_empty() || s.len() > 512 || s.chars().any(char::is_control))
        {
            return Err("invalid Session namespace".into());
        }
        Ok(())
    }
}
impl lenso::TypedExtension for SessionNamespace {
    const KEY: &'static str = "lenso.agent.session-namespace@1";
}
