//! Console proxy authentication. User identity is exclusively an Auth signature.
use super::{ApiProblem, RUN_TURN_OPERATION};
use axum::http::HeaderMap;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use lenso_auth_sdk::{ActorAssertion, ActorAssertionVerifier, AuthOutcome, TypedActor};
use lenso_kernel::{CancellationToken, InvocationContext};

#[derive(Clone, Default)]
pub(crate) struct RequestIdentity {
    pub(crate) actor: Option<ActorAssertion>,
}
impl std::fmt::Debug for RequestIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestIdentity")
            .field("owner", &self.owner())
            .finish()
    }
}
impl RequestIdentity {
    pub(crate) fn owner(&self) -> String {
        self.actor.as_ref().map_or_else(
            || "local".into(),
            |actor| serde_json::json!([actor.issuer(), actor.subject()]).to_string(),
        )
    }
    pub(crate) fn key(&self, request: &str) -> (String, String) {
        (self.owner(), request.to_owned())
    }
}
struct User;
impl TypedActor for User {
    fn from_assertion(
        assertion: &ActorAssertion,
    ) -> Result<Self, lenso_auth_sdk::ActorProjectionError> {
        if assertion.actor_kind() != "user" {
            return Err(lenso_auth_sdk::ActorProjectionError::UnexpectedActorKind {
                expected: "user".into(),
                actual: assertion.actor_kind().into(),
            });
        }
        Ok(Self)
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Authority(ActorAssertionVerifier);
impl Authority {
    pub(crate) fn new(issuer: &str, public_key: &str) -> Result<Self, String> {
        ActorAssertionVerifier::from_public_key_base64(issuer, public_key)
            .map(Self)
            .map_err(|_| "invalid Agent assertion authority".into())
    }
    pub(crate) fn identity(&self, headers: &HeaderMap) -> Result<RequestIdentity, ApiProblem> {
        let denied = || ApiProblem::forbidden("A valid Console user assertion is required");
        let encoded = headers
            .get("x-lenso-actor-assertion")
            .and_then(|h| h.to_str().ok())
            .filter(|s| !s.is_empty() && s.len() <= 16384)
            .ok_or_else(denied)?;
        let wire = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(encoded).map_err(|_| denied())?)
            .map_err(|_| denied())?;
        let AuthOutcome::Authenticated(actor) =
            lenso_auth_sdk::decode_auth_response(lenso_capability_auth::AuthResponse {
                kind: lenso_capability_auth::AuthResponseKind::Authenticated,
                assertion: Some(wire),
            })
            .map_err(|_| denied())?
        else {
            return Err(denied());
        };
        let context = actor
            .clone()
            .attach(InvocationContext::new(1, None, CancellationToken::new()))
            .map_err(|_| denied())?;
        self.0
            .project_context::<User>(
                &context,
                lenso_capability_agent::CAPABILITY_ID,
                RUN_TURN_OPERATION,
                &lenso_auth_sdk::FixedClock::new(time::OffsetDateTime::now_utc()),
            )
            .map_err(|_| denied())?;
        Ok(RequestIdentity { actor: Some(actor) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lenso_auth_sdk::{ActorAssertionIssuer, Validity, audience};
    use std::collections::BTreeMap;
    fn signed(
        issuer: &ActorAssertionIssuer,
        subject: &str,
        kind: &str,
        targets: Vec<String>,
        expired: bool,
    ) -> HeaderMap {
        let now = time::OffsetDateTime::now_utc();
        let actor = issuer.issue(
            subject,
            kind,
            "synthetic",
            targets,
            Validity::new(
                now - time::Duration::minutes(2),
                if expired {
                    now - time::Duration::minutes(1)
                } else {
                    now + time::Duration::minutes(5)
                },
            )
            .unwrap(),
            BTreeMap::new(),
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-lenso-actor-assertion",
            URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&actor.to_wire()).unwrap())
                .parse()
                .unwrap(),
        );
        headers
    }
    #[test]
    fn unsigned_subject_wrong_authority_audience_kind_and_expiry_are_rejected() {
        let issuer = ActorAssertionIssuer::from_signing_key("fixture", [41; 32]);
        let authority = Authority::new("fixture", &issuer.public_key_base64()).unwrap();
        let target = vec![audience(
            lenso_capability_agent::CAPABILITY_ID,
            RUN_TURN_OPERATION,
        )];
        let alice = authority
            .identity(&signed(&issuer, "alice", "user", target.clone(), false))
            .unwrap();
        assert_eq!(alice.owner(), r#"["fixture","alice"]"#);
        let mut unsigned = HeaderMap::new();
        unsigned.insert("x-lenso-subject", "alice".parse().unwrap());
        assert!(authority.identity(&unsigned).is_err());
        let foreign = ActorAssertionIssuer::from_signing_key("fixture", [42; 32]);
        assert!(
            authority
                .identity(&signed(&foreign, "alice", "user", target.clone(), false))
                .is_err()
        );
        assert!(
            authority
                .identity(&signed(&issuer, "alice", "service", target.clone(), false))
                .is_err()
        );
        assert!(
            authority
                .identity(&signed(
                    &issuer,
                    "alice",
                    "user",
                    vec!["unrelated".into()],
                    false
                ))
                .is_err()
        );
        assert!(
            authority
                .identity(&signed(&issuer, "alice", "user", target, true))
                .is_err()
        );
    }
    #[test]
    fn tampered_subject_fails_and_request_keys_are_owner_scoped() {
        let issuer = ActorAssertionIssuer::from_signing_key("fixture", [41; 32]);
        let authority = Authority::new("fixture", &issuer.public_key_base64()).unwrap();
        let target = vec![audience(
            lenso_capability_agent::CAPABILITY_ID,
            RUN_TURN_OPERATION,
        )];
        let alice_headers = signed(&issuer, "alice", "user", target.clone(), false);
        let alice = authority.identity(&alice_headers).unwrap();
        let bob = authority
            .identity(&signed(&issuer, "bob", "user", target, false))
            .unwrap();
        assert_ne!(alice.key("same-id"), bob.key("same-id"));
        let mut wire: serde_json::Value = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(alice_headers["x-lenso-actor-assertion"].to_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        wire["subject"] = "bob".into();
        let mut tampered = HeaderMap::new();
        tampered.insert(
            "x-lenso-actor-assertion",
            URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&wire).unwrap())
                .parse()
                .unwrap(),
        );
        assert!(authority.identity(&tampered).is_err());
    }
}
