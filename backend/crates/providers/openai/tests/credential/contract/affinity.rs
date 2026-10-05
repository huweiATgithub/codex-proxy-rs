//! 验证会话账号绑定的发送前裁决、故障边界与并发收敛

use super::*;

#[tokio::test]
async fn concurrent_initial_requests_converge_before_either_response_completes() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first", "test-first");
    create_account(&store, "acct_second", "test-second");
    let leases = Arc::new(TestLeaseCoordinator::default());
    let affinity = Arc::new(MemorySessionAffinity::default());
    affinity.synchronize_initial_claims(2);
    let selector = selector_with_affinity(&store, leases.clone(), affinity.clone());
    let provider = ProviderKind::new("openai").unwrap();
    let key = ProviderSessionAffinityKey::try_new("concurrent-root-and-child").unwrap();
    let root_attempt = round_robin_attempt();
    let child_attempt = round_robin_attempt();
    let url = Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap();
    let root = SelectCodexCredential {
        upstream_model: "gpt-5.4",
        request_url: &url,
        attempt: &root_attempt,
        session_affinity_key: Some(&key),
    };
    let child = SelectCodexCredential {
        attempt: &child_attempt,
        ..root
    };

    let (root_selected, child_selected) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(selector.select(&root), selector.select(&child))
    })
    .await
    .expect("concurrent binding decision completes");
    let root_selected = root_selected.expect("root account");
    let child_selected = child_selected.expect("child account");
    let binding = affinity.load(&provider, &key).await.unwrap().unwrap();

    assert_eq!(root_selected.account_id(), binding.account_id());
    assert_eq!(child_selected.account_id(), binding.account_id());
    assert_eq!(affinity.binding_count(), 1);
    assert_eq!(
        affinity.renewal_ttls(),
        vec![Duration::from_secs(7 * 24 * 60 * 60); 2]
    );
    let candidates = leases
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.account_id().clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        candidates.len(),
        2,
        "both candidates participated in the initial race"
    );
}

#[tokio::test]
async fn affinity_store_failures_prevent_selection_without_falling_back() {
    for failure_during_load in [true, false] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_first", "test-first");
        create_account(&store, "acct_second", "test-second");
        let affinity = Arc::new(MemorySessionAffinity::default());
        if failure_during_load {
            affinity.fail_load();
        } else {
            affinity.fail_update();
        }
        let leases = Arc::new(TestLeaseCoordinator::default());
        let selector = selector_with_affinity(&store, leases.clone(), affinity.clone());
        let key = ProviderSessionAffinityKey::try_new("unavailable-binding-store").unwrap();
        let request_attempt = attempt(BTreeSet::new());
        let url = Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap();

        let result = selector
            .select(&SelectCodexCredential {
                upstream_model: "gpt-5.4",
                request_url: &url,
                attempt: &request_attempt,
                session_affinity_key: Some(&key),
            })
            .await;

        assert!(
            matches!(result, Err(CredentialSelectionError::Store)),
            "unconfirmed owner must fail with a store error before dispatch"
        );
        assert_eq!(affinity.binding_count(), 0);
        assert!(affinity.renewal_ttls().is_empty());
        if failure_during_load {
            assert!(leases.requests.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn confirmed_account_inaccessibility_switches_the_binding_before_dispatch() {
    for reason in [
        "disabled",
        "deleted",
        "banned",
        "verification",
        "credential_expired",
    ] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_first", "test-first");
        create_account(&store, "acct_second", "test-second");
        let first = store.account("acct_first").unwrap();
        let provider = ProviderKind::new("openai").unwrap();
        let key = ProviderSessionAffinityKey::try_new("confirmed-inaccessibility").unwrap();
        let affinity = Arc::new(MemorySessionAffinity::default());
        affinity.seed_binding(&provider, key.expose_to_store(), first.id().clone());
        let initial = affinity.load(&provider, &key).await.unwrap().unwrap();
        let selector = selector_with_affinity(
            &store,
            Arc::new(TestLeaseCoordinator::default()),
            affinity.clone(),
        );
        match reason {
            "disabled" => store.set_enabled(first.id(), false).await.unwrap(),
            "deleted" => store.delete_account(first.id()).await.unwrap(),
            "banned" => selector
                .record_failure(&first, CodexAccountFailure::Banned, None)
                .await
                .unwrap(),
            "verification" => selector
                .record_failure(
                    &first,
                    CodexAccountFailure::IdentityVerificationRequired,
                    None,
                )
                .await
                .unwrap(),
            "credential_expired" => store
                .apply_state_change(AccountStateChange {
                    account_id: first.id().clone(),
                    expected_revision: first.revision(),
                    credential_state: CredentialState::Expired,
                    observed_at: SystemTime::now(),
                    error_reason: Some(AccountErrorReason::CredentialExpired),
                    message: None,
                })
                .await
                .unwrap(),
            _ => unreachable!(),
        }
        let request_attempt = attempt(BTreeSet::new());
        let url = Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap();

        let selected = selector
            .select(&SelectCodexCredential {
                upstream_model: "gpt-5.4",
                request_url: &url,
                attempt: &request_attempt,
                session_affinity_key: Some(&key),
            })
            .await
            .unwrap_or_else(|error| panic!("{reason}: {error}"));
        let current = affinity.load(&provider, &key).await.unwrap().unwrap();

        assert_eq!(selected.account_id().as_str(), "acct_second", "{reason}");
        assert_eq!(current.account_id(), selected.account_id(), "{reason}");
        assert_ne!(current.token(), initial.token(), "{reason}");
        assert!(selected.account_switch(), "{reason}");
    }
}

#[tokio::test]
async fn temporary_rejection_and_generic_invalid_credentials_do_not_switch_accounts() {
    for reason in ["rate_limit", "cloudflare_path", "credential_invalid"] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_first", "test-first");
        create_account(&store, "acct_second", "test-second");
        let first = store.account("acct_first").unwrap();
        let provider = ProviderKind::new("openai").unwrap();
        let key = ProviderSessionAffinityKey::try_new("transient-account-failure").unwrap();
        let affinity = Arc::new(MemorySessionAffinity::default());
        affinity.seed_binding(&provider, key.expose_to_store(), first.id().clone());
        let initial = affinity.load(&provider, &key).await.unwrap();
        let leases = Arc::new(TestLeaseCoordinator::default());
        let selector = selector_with_affinity(&store, leases.clone(), affinity.clone());
        match reason {
            "rate_limit" => selector
                .record_failure(
                    &first,
                    CodexAccountFailure::RateLimited {
                        retry_after: Some(Duration::from_secs(30)),
                    },
                    None,
                )
                .await
                .unwrap(),
            "cloudflare_path" => {
                for _ in 0..3 {
                    selector
                        .record_failure(&first, CodexAccountFailure::CloudflarePathBlocked, None)
                        .await
                        .unwrap();
                }
            }
            "credential_invalid" => {
                persist_credential_state(&store, &first, CredentialState::Invalid)
            }
            _ => unreachable!(),
        }
        let request_attempt = attempt(BTreeSet::new());
        let url = Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap();

        let result = selector
            .select(&SelectCodexCredential {
                upstream_model: "gpt-5.4",
                request_url: &url,
                attempt: &request_attempt,
                session_affinity_key: Some(&key),
            })
            .await;

        assert!(result.is_err(), "{reason} must preserve unavailable owner");
        assert_eq!(
            affinity.load(&provider, &key).await.unwrap(),
            initial,
            "{reason}"
        );
        assert!(affinity.renewal_ttls().is_empty(), "{reason}");
        assert!(
            leases
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| request.account_id() == first.id()),
            "{reason}"
        );
    }
}

#[tokio::test]
async fn unavailable_owner_without_a_replacement_preserves_its_binding() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first", "test-first");
    let first = store.account("acct_first").unwrap();
    let provider = ProviderKind::new("openai").unwrap();
    let key = ProviderSessionAffinityKey::try_new("no-replacement").unwrap();
    let affinity = Arc::new(MemorySessionAffinity::default());
    affinity.seed_binding(&provider, key.expose_to_store(), first.id().clone());
    let initial = affinity.load(&provider, &key).await.unwrap();
    store.set_enabled(first.id(), false).await.unwrap();
    let selector = selector_with_affinity(
        &store,
        Arc::new(TestLeaseCoordinator::default()),
        affinity.clone(),
    );
    let request_attempt = attempt(BTreeSet::new());
    let url = Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap();

    let result = selector
        .select(&SelectCodexCredential {
            upstream_model: "gpt-5.4",
            request_url: &url,
            attempt: &request_attempt,
            session_affinity_key: Some(&key),
        })
        .await;

    assert!(result.is_err());
    assert_eq!(affinity.load(&provider, &key).await.unwrap(), initial);
    assert!(affinity.renewal_ttls().is_empty());
}

#[tokio::test]
async fn successful_response_does_not_create_an_absent_binding() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first", "test-first");
    let affinity = Arc::new(MemorySessionAffinity::default());
    let selector = selector_with_affinity(
        &store,
        Arc::new(TestLeaseCoordinator::default()),
        affinity.clone(),
    );

    selector
        .record_success(&store.account("acct_first").unwrap())
        .await;

    assert_eq!(affinity.binding_count(), 0);
    assert!(affinity.renewal_ttls().is_empty());
}

#[tokio::test]
async fn late_success_on_previous_account_does_not_trigger_switchback() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first", "test-first");
    create_account(&store, "acct_second", "test-second");
    store.set_scheduling("acct_first", None, AccountWeight::new(100).unwrap());
    let first = store.account("acct_first").unwrap();
    let provider = ProviderKind::new("openai").unwrap();
    let key = ProviderSessionAffinityKey::try_new("late-success-after-switch").unwrap();
    let affinity = Arc::new(MemorySessionAffinity::default());
    affinity.seed_binding(&provider, key.expose_to_store(), first.id().clone());
    let selector = selector_with_affinity(
        &store,
        Arc::new(TestLeaseCoordinator::default()),
        affinity.clone(),
    );
    selector
        .record_failure(&first, CodexAccountFailure::Banned, None)
        .await
        .unwrap();
    let request_attempt = attempt(BTreeSet::new());
    let url = Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap();
    let request = SelectCodexCredential {
        upstream_model: "gpt-5.4",
        request_url: &url,
        attempt: &request_attempt,
        session_affinity_key: Some(&key),
    };
    let migrated = selector.select(&request).await.unwrap();
    let binding_after_switch = affinity.load(&provider, &key).await.unwrap();
    let renewals_after_switch = affinity.renewal_ttls();
    let previous = store.account("acct_first").unwrap();

    selector.record_success(&previous).await;

    assert_eq!(
        affinity.load(&provider, &key).await.unwrap(),
        binding_after_switch
    );
    assert_eq!(affinity.renewal_ttls(), renewals_after_switch);
    assert_eq!(
        store.account("acct_first").unwrap().credential_state(),
        CredentialState::Ready
    );
    let next = selector.select(&request).await.unwrap();
    assert_eq!(next.account_id(), migrated.account_id());
    assert_eq!(next.account_id().as_str(), "acct_second");
}

#[tokio::test]
async fn expired_access_token_switches_only_when_same_account_refresh_is_unavailable() {
    for has_refresh_token in [false, true] {
        let store = Arc::new(MemoryAccountStore::default());
        let mut expired_profile = profile("chatgpt-acct_first");
        expired_profile.access_token_expires_at =
            Some(chrono::Utc::now() - chrono::Duration::minutes(1));
        let mut credential = secret("expired-first");
        if !has_refresh_token {
            credential.refresh_token = None;
        }
        store
            .seed_oauth_credential(ImportCodexOAuthCredential {
                account_id: "acct_first".to_owned(),
                name: "acct_first".to_owned(),
                secret: credential,
                verified_account: expired_profile,
                next_refresh_at: has_refresh_token.then(chrono::Utc::now),
                enabled: true,
            })
            .await;
        create_account(&store, "acct_second", "test-second");
        let first = store.account("acct_first").unwrap();
        assert_eq!(first.credential_state(), CredentialState::Ready);
        let provider = ProviderKind::new("openai").unwrap();
        let key = ProviderSessionAffinityKey::try_new("expired-access-token").unwrap();
        let affinity = Arc::new(MemorySessionAffinity::default());
        affinity.seed_binding(&provider, key.expose_to_store(), first.id().clone());
        let initial_binding = affinity.load(&provider, &key).await.unwrap().unwrap();
        let leases = Arc::new(TestLeaseCoordinator::default());
        let selector = selector_with_affinity(&store, leases.clone(), affinity.clone());
        let request_attempt = attempt(BTreeSet::new());
        let url = Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap();

        let result = selector
            .select(&SelectCodexCredential {
                upstream_model: "gpt-5.4",
                request_url: &url,
                attempt: &request_attempt,
                session_affinity_key: Some(&key),
            })
            .await;
        let current_binding = affinity.load(&provider, &key).await.unwrap().unwrap();

        if has_refresh_token {
            // 过期令牌在刷新完成前不能发送，但可恢复凭据仍保留会话 owner
            assert!(matches!(
                result,
                Err(CredentialSelectionError::NoEligibleCredential)
            ));
            assert_eq!(current_binding, initial_binding);
            assert!(leases.requests.lock().unwrap().is_empty());
            assert!(affinity.renewal_ttls().is_empty());
        } else {
            let selected = result.expect("unrecoverable owner switches to available account");
            assert_eq!(selected.account_id().as_str(), "acct_second");
            assert_eq!(current_binding.account_id(), selected.account_id());
            assert_ne!(current_binding.token(), initial_binding.token());
            assert!(selected.account_switch());
        }
    }
}
