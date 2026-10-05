//! 验证会话账号决策、发送前存储边界与绑定值对象

use std::sync::Mutex;
use std::time::Duration;

use futures::{executor::block_on, future::BoxFuture};
use gateway_core::account::ProviderAccountId;
use gateway_core::account::affinity::{
    AccountBinding, AffinityAccountState, AffinityResolution, AffinitySnapshot, AffinityStore,
    AffinityStoreError, AffinityStoreErrorKind, AffinitySwitchReason, AffinityUpdate, BindingToken,
    SessionAffinityKey, resolve,
};
use gateway_core::identity::ProviderKind;

#[test]
fn absent_session_admission_should_require_absence_and_use_seven_day_ttl() {
    block_on(async {
        let provider = ProviderKind::new("openai").expect("provider");
        let key = SessionAffinityKey::try_new("session-key").expect("key");
        let candidate = binding("acct_selected", 'a');
        let expected = AffinityUpdate::Applied(candidate.clone());
        let store = RecordingStore::new(Ok(expected.clone()));

        let result = resolve(AffinitySnapshot::Absent)
            .admit(&store, &provider, &key, candidate.account_id())
            .await
            .expect("admit initial binding");

        assert_eq!(result, expected);
        assert_eq!(
            store.calls.into_inner().expect("recorded calls"),
            vec![AdmissionCall {
                provider,
                key,
                expected: None,
                selected: candidate.account_id().clone(),
                ttl: Duration::from_secs(7 * 24 * 60 * 60),
            }]
        );
    });
}

#[test]
fn retained_owner_admission_should_compare_the_exact_binding_and_renew_it() {
    block_on(async {
        let provider = ProviderKind::new("openai").expect("provider");
        let key = SessionAffinityKey::try_new("session-key").expect("key");
        let current = binding("acct_current", 'a');
        let expected = AffinityUpdate::Applied(current.clone());
        let store = RecordingStore::new(Ok(expected.clone()));

        let result = resolve(AffinitySnapshot::Bound {
            binding: &current,
            state: AffinityAccountState::Retain,
        })
        .admit(&store, &provider, &key, current.account_id())
        .await
        .expect("retain current owner");

        assert_eq!(result, expected);
        assert_eq!(
            store.calls.into_inner().expect("recorded calls"),
            vec![AdmissionCall {
                provider,
                key,
                selected: current.account_id().clone(),
                expected: Some(current),
                ttl: Duration::from_secs(7 * 24 * 60 * 60),
            }]
        );
    });
}

#[test]
fn confirmed_switch_reasons_should_preserve_the_observed_generation_until_admission() {
    block_on(async {
        let provider = ProviderKind::new("openai").expect("provider");
        let key = SessionAffinityKey::try_new("session-key").expect("key");
        let observed = binding("acct_previous", 'a');
        let replacement = binding("acct_replacement", 'b');
        for reason in [
            AffinitySwitchReason::QuotaExhausted,
            AffinitySwitchReason::CredentialUnrecoverable,
            AffinitySwitchReason::AccountBlocked,
            AffinitySwitchReason::AccountVerificationRequired,
            AffinitySwitchReason::AccountDisabled,
            AffinitySwitchReason::AccountDeleted,
        ] {
            let expected = AffinityUpdate::Applied(replacement.clone());
            let store = RecordingStore::new(Ok(expected.clone()));
            let resolution = resolve(AffinitySnapshot::Bound {
                binding: &observed,
                state: AffinityAccountState::Switch(reason),
            });
            assert_eq!(
                resolution,
                AffinityResolution::Switch {
                    binding: &observed,
                    reason,
                }
            );

            assert_eq!(
                resolution
                    .admit(&store, &provider, &key, replacement.account_id())
                    .await
                    .expect("admit allowed switch"),
                expected
            );
            assert_eq!(
                store.calls.into_inner().expect("recorded calls"),
                vec![AdmissionCall {
                    provider: provider.clone(),
                    key: key.clone(),
                    expected: Some(observed.clone()),
                    selected: replacement.account_id().clone(),
                    ttl: Duration::from_secs(7 * 24 * 60 * 60),
                }]
            );
        }
    });
}

#[test]
fn retained_owner_admission_should_reject_other_accounts_before_accessing_storage() {
    block_on(async {
        let provider = ProviderKind::new("openai").expect("provider");
        let key = SessionAffinityKey::try_new("session-key").expect("key");
        let current = binding("acct_current", 'a');
        let other = binding("acct_other", 'b');
        let store = RecordingStore::new(Ok(AffinityUpdate::Applied(other.clone())));

        let error = resolve(AffinitySnapshot::Bound {
            binding: &current,
            state: AffinityAccountState::Retain,
        })
        .admit(&store, &provider, &key, other.account_id())
        .await
        .expect_err("retained owner cannot be bypassed");

        assert_eq!(error.kind(), AffinityStoreErrorKind::InvalidData);
        assert!(store.calls.into_inner().expect("recorded calls").is_empty());
    });
}

#[test]
fn request_incompatibility_should_reject_any_account_without_mutating_binding() {
    block_on(async {
        let provider = ProviderKind::new("openai").expect("provider");
        let key = SessionAffinityKey::try_new("session-key").expect("key");
        let current = binding("acct_current", 'a');
        let other = binding("acct_other", 'b');
        for selected in [current.account_id(), other.account_id()] {
            let store = RecordingStore::new(Ok(AffinityUpdate::Applied(other.clone())));
            let resolution = resolve(AffinitySnapshot::Bound {
                binding: &current,
                state: AffinityAccountState::RequestIncompatible,
            });
            assert_eq!(resolution, AffinityResolution::Reject(&current));

            let error = resolution
                .admit(&store, &provider, &key, selected)
                .await
                .expect_err("incompatible request cannot be admitted");

            assert_eq!(error.kind(), AffinityStoreErrorKind::InvalidData);
            assert!(store.calls.into_inner().expect("recorded calls").is_empty());
        }
    });
}

#[test]
fn admission_should_return_conflicts_and_storage_errors_without_retrying_or_rebinding() {
    block_on(async {
        let provider = ProviderKind::new("openai").expect("provider");
        let key = SessionAffinityKey::try_new("session-key").expect("key");
        let observed = binding("acct_current", 'a');
        let newer_generation = binding("acct_current", 'b');
        for expected in [
            Ok(AffinityUpdate::Conflict(None)),
            Ok(AffinityUpdate::Conflict(Some(newer_generation))),
            Err(AffinityStoreError::new(
                AffinityStoreErrorKind::Unavailable,
                "uncertain write result",
            )),
            Err(AffinityStoreError::new(
                AffinityStoreErrorKind::InvalidData,
                "decode stored binding",
            )),
        ] {
            let store = RecordingStore::new(expected.clone());

            let result = resolve(AffinitySnapshot::Bound {
                binding: &observed,
                state: AffinityAccountState::Retain,
            })
            .admit(&store, &provider, &key, observed.account_id())
            .await;

            assert_eq!(result, expected);
            assert_eq!(store.calls.into_inner().expect("recorded calls").len(), 1);
        }
    });
}

#[test]
fn affinity_key_should_reject_unbounded_or_noncanonical_identity() {
    for invalid in [
        String::new(),
        "a".repeat(129),
        "Session".to_owned(),
        "session key".to_owned(),
        "session:key".to_owned(),
        "会话".to_owned(),
    ] {
        assert!(SessionAffinityKey::try_new(invalid).is_err());
    }
    let maximum_length = "a".repeat(128);
    assert_eq!(
        SessionAffinityKey::try_new(maximum_length.clone())
            .expect("bounded key")
            .expose_to_store(),
        maximum_length
    );
}

#[test]
fn binding_token_should_reject_malformed_generation_values() {
    for invalid in [
        String::new(),
        "a".repeat(31),
        "a".repeat(33),
        "A".repeat(32),
        "g".repeat(32),
        " ".repeat(32),
    ] {
        assert!(BindingToken::try_new(invalid).is_err());
    }
    let value = "0123456789abcdef0123456789abcdef";
    assert_eq!(
        BindingToken::try_new(value)
            .expect("canonical token")
            .expose_to_store(),
        value
    );
}

#[test]
fn binding_token_should_remain_opaque_in_binding_debug_output() {
    let binding = binding("acct_current", 'a');

    assert!(!format!("{:?}", binding.token()).contains(binding.token().expose_to_store()));
    assert!(!format!("{binding:?}").contains(binding.token().expose_to_store()));
}

fn binding(account: &str, token_digit: char) -> AccountBinding {
    AccountBinding::new(
        ProviderAccountId::new(account).expect("account"),
        BindingToken::try_new(token_digit.to_string().repeat(32)).expect("token"),
    )
}

#[derive(Debug, PartialEq, Eq)]
struct AdmissionCall {
    provider: ProviderKind,
    key: SessionAffinityKey,
    expected: Option<AccountBinding>,
    selected: ProviderAccountId,
    ttl: Duration,
}

struct RecordingStore {
    calls: Mutex<Vec<AdmissionCall>>,
    outcome: Result<AffinityUpdate, AffinityStoreError>,
}

impl RecordingStore {
    fn new(outcome: Result<AffinityUpdate, AffinityStoreError>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            outcome,
        }
    }
}

impl AffinityStore for RecordingStore {
    fn load<'a>(
        &'a self,
        _provider: &'a ProviderKind,
        _key: &'a SessionAffinityKey,
    ) -> BoxFuture<'a, Result<Option<AccountBinding>, AffinityStoreError>> {
        Box::pin(async {
            panic!("admission must compare its original observation without reloading")
        })
    }

    fn compare_and_set<'a>(
        &'a self,
        provider: &'a ProviderKind,
        key: &'a SessionAffinityKey,
        expected: Option<&'a AccountBinding>,
        selected_account: &'a ProviderAccountId,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<AffinityUpdate, AffinityStoreError>> {
        Box::pin(async move {
            self.calls
                .lock()
                .expect("recorded calls")
                .push(AdmissionCall {
                    provider: provider.clone(),
                    key: key.clone(),
                    expected: expected.cloned(),
                    selected: selected_account.clone(),
                    ttl,
                });
            self.outcome.clone()
        })
    }
}
