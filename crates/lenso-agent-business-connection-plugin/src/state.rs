use super::{auth, binding, failure};
use lenso_kernel::{CancellationToken, InvocationContext, RuntimeFailure};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

#[derive(Default)]
pub(super) struct State {
    pub grant: Option<Arc<Grant>>,
    pub attempt: Option<Attempt>,
    pub turns: BTreeMap<uuid::Uuid, Turn>,
}
impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BusinessConnectionState")
            .finish_non_exhaustive()
    }
}
pub(super) struct Turn {
    pub grant: Option<Arc<Grant>>,
    pub cancellation: CancellationToken,
    pub created: Instant,
}
impl State {
    pub fn prune(&mut self) {
        self.turns.retain(|_, turn| {
            !turn.cancellation.is_cancelled() && turn.created.elapsed() < Duration::from_secs(3600)
        });
    }
    pub fn for_context(
        &mut self,
        context: &InvocationContext,
        required: bool,
    ) -> Result<Option<Arc<Grant>>, RuntimeFailure> {
        self.prune();
        if context.is_cancelled() {
            return Err(failure());
        }
        let Some(scope) = context.extension(binding::SCOPE_EXTENSION) else {
            return if required {
                Err(failure())
            } else {
                Ok(self.grant.clone().filter(|grant| grant.valid()))
            };
        };
        let id = std::str::from_utf8(scope)
            .ok()
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .ok_or_else(failure)?;
        let turn = self.turns.get(&id).ok_or_else(failure)?;
        Ok(turn.grant.clone().filter(|grant| grant.valid()))
    }
}
#[derive(Deserialize)]
pub(super) struct Begin {
    pub attempt_id: String,
    pub polling_secret: String,
    pub authorization_url: String,
    pub expires_at_millis: String,
}
pub(super) struct Attempt {
    pub id: String,
    pub remote_id: String,
    pub secret: Zeroizing<String>,
    pub authorization_url: String,
    pub expires: u64,
    pub connected: bool,
    pub failed: bool,
    pub cancellation: CancellationToken,
}
impl Drop for Attempt {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}
impl Attempt {
    pub fn new(begin: Begin, origin: &str) -> Result<Self, RuntimeFailure> {
        let url = reqwest::Url::parse(&begin.authorization_url).map_err(|_| failure())?;
        let expires = begin
            .expires_at_millis
            .parse::<u64>()
            .map_err(|_| failure())?;
        if url.origin().ascii_serialization() != origin
            || url.path() != "/auth/agent/authorize"
            || !url.username().is_empty()
            || url.password().is_some()
            || begin.attempt_id.len() > 128
            || begin.polling_secret.is_empty()
            || begin.polling_secret.len() > 512
            || expires <= now()
            || expires > now().saturating_add(600_000)
        {
            return Err(failure());
        }
        Ok(Self {
            id: uuid::Uuid::new_v4().to_string(),
            remote_id: begin.attempt_id,
            secret: Zeroizing::new(begin.polling_secret),
            authorization_url: begin.authorization_url,
            expires,
            connected: false,
            failed: false,
            cancellation: CancellationToken::new(),
        })
    }
    pub fn valid(&self) -> bool {
        self.expires > now()
    }
    pub fn presentation(&self) -> auth::BeginResponse {
        auth::BeginResponse {
            attempt_id: self.id.clone(),
            authorization_url: self.authorization_url.clone(),
            user_code: String::new(),
            expires_at_millis: self.expires.to_string(),
        }
    }
}
#[derive(Deserialize)]
pub(super) struct Poll {
    pub state: String,
    pub grant: Option<RemoteGrant>,
}
#[derive(Deserialize)]
pub(super) struct RemoteGrant {
    pub credential: String,
    pub expires_at: String,
}
pub(super) struct Grant {
    pub credential: Zeroizing<String>,
    expires: i128,
}
impl Grant {
    pub fn new(grant: RemoteGrant) -> Result<Self, RuntimeFailure> {
        let expires = time::OffsetDateTime::parse(
            &grant.expires_at,
            &time::format_description::well_known::Rfc3339,
        )
        .map_err(|_| failure())?
        .unix_timestamp_nanos()
            / 1_000_000;
        if grant.credential.is_empty()
            || grant.credential.len() > 512
            || expires <= i128::from(now())
            || expires > i128::from(now()) + 3_600_000
        {
            return Err(failure());
        }
        Ok(Self {
            credential: Zeroizing::new(grant.credential),
            expires,
        })
    }
    pub fn valid(&self) -> bool {
        self.expires > i128::from(now())
    }
}
pub(super) fn now() -> u64 {
    u64::try_from(time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn grant(secret: &str) -> Arc<Grant> {
        Arc::new(Grant {
            credential: Zeroizing::new(secret.into()),
            expires: i128::from(now()) + 60_000,
        })
    }
    fn context(id: uuid::Uuid) -> InvocationContext {
        InvocationContext::new(1, None, CancellationToken::new())
            .with_extension(binding::SCOPE_EXTENSION, id.to_string().into_bytes())
            .unwrap()
    }
    #[test]
    fn switching_and_disconnecting_never_changes_admitted_identity() {
        let id = uuid::Uuid::new_v4();
        let cancellation = CancellationToken::new();
        let old = grant("account-a");
        let mut state = State {
            grant: Some(grant("account-b")),
            ..State::default()
        };
        state.turns.insert(
            id,
            Turn {
                grant: Some(old.clone()),
                cancellation: cancellation.clone(),
                created: Instant::now(),
            },
        );
        assert_eq!(
            state
                .for_context(&context(id), true)
                .unwrap()
                .unwrap()
                .credential
                .as_str(),
            "account-a"
        );
        state.grant = None;
        assert_eq!(
            state
                .for_context(&context(id), true)
                .unwrap()
                .unwrap()
                .credential
                .as_str(),
            "account-a"
        );
        cancellation.cancel();
        assert!(state.for_context(&context(id), true).is_err());
        assert_eq!(Arc::strong_count(&old), 1);
        assert!(!format!("{state:?}").contains("account-a"));
    }
    #[test]
    fn unknown_absent_and_expired_scope_cannot_select_current_account() {
        let id = uuid::Uuid::new_v4();
        let mut state = State {
            grant: Some(grant("current")),
            ..State::default()
        };
        assert!(state.for_context(&context(id), true).is_err());
        assert!(
            state
                .for_context(
                    &InvocationContext::new(1, None, CancellationToken::new()),
                    true
                )
                .is_err()
        );
        state.turns.insert(
            id,
            Turn {
                grant: None,
                cancellation: CancellationToken::new(),
                created: Instant::now(),
            },
        );
        assert!(state.for_context(&context(id), true).unwrap().is_none());
        state.turns.get_mut(&id).unwrap().created = Instant::now()
            .checked_sub(Duration::from_secs(3601))
            .unwrap();
        assert!(state.for_context(&context(id), true).is_err());
    }
    #[test]
    fn browser_handoff_rejects_foreign_origin_and_excessive_lifetime() {
        for (url, expires) in [
            ("https://other.example/auth/agent/authorize", now() + 30_000),
            ("https://app.example/auth/agent/authorize", now() + 700_000),
        ] {
            assert!(
                Attempt::new(
                    Begin {
                        attempt_id: "remote".into(),
                        polling_secret: "private".into(),
                        authorization_url: url.into(),
                        expires_at_millis: expires.to_string()
                    },
                    "https://app.example"
                )
                .is_err()
            );
        }
    }
}
