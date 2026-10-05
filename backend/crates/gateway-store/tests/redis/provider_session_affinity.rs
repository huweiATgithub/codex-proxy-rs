//! 验证账号绑定的原子裁决、代次隔离、有效期与存储失败边界

use gateway_core::account::ProviderAccountId;
use gateway_core::account::affinity::{
    ACCOUNT_BINDING_TTL, AccountBinding, AffinityStore, AffinityStoreErrorKind, AffinityUpdate,
    SessionAffinityKey,
};
use gateway_core::identity::ProviderKind;
use gateway_store::redis::RedisProviderSessionAffinityRepository;
use redis::aio::ConnectionManager;
use uuid::Uuid;

#[tokio::test]
async fn session_affinity_should_isolate_providers_and_sessions_without_exposing_raw_keys() {
    let Some((repository, mut connection, namespace)) = affinity_repository().await else {
        return;
    };
    let first_provider = ProviderKind::new("openai").expect("provider");
    let second_provider = ProviderKind::new("other").expect("provider");
    let first_key = SessionAffinityKey::try_new("session-secret-value").expect("affinity key");
    let second_key = SessionAffinityKey::try_new("other-session-secret").expect("affinity key");
    let first_account = ProviderAccountId::new("acct_first").expect("account");
    let second_account = ProviderAccountId::new("acct_second").expect("account");
    for (provider, key, account) in [
        (&first_provider, &first_key, &first_account),
        (&second_provider, &first_key, &second_account),
        (&first_provider, &second_key, &second_account),
    ] {
        let binding = applied(
            repository
                .compare_and_set(provider, key, None, account, ACCOUNT_BINDING_TTL)
                .await
                .expect("create independent binding"),
        );
        assert_eq!(binding.account_id(), account);
        assert_eq!(
            repository.load(provider, key).await.expect("load binding"),
            Some(binding)
        );
    }
    let keys = namespace_keys(&mut connection, &namespace).await;
    assert_eq!(keys.len(), 3);
    assert!(keys.iter().all(|key| {
        !key.contains("session-secret-value") && !key.contains("other-session-secret")
    }));
}

#[tokio::test]
async fn session_affinity_concurrent_claims_should_return_one_shared_winner() {
    let Some((repository, _connection, _namespace)) = affinity_repository().await else {
        return;
    };
    let other_repository = repository.clone();
    let provider = ProviderKind::new("openai").expect("provider");
    let key = SessionAffinityKey::try_new("concurrent-claim").expect("affinity key");
    let first = ProviderAccountId::new("acct_first").expect("account");
    let second = ProviderAccountId::new("acct_second").expect("account");

    let (first_result, second_result) = tokio::join!(
        repository.compare_and_set(&provider, &key, None, &first, ACCOUNT_BINDING_TTL),
        other_repository.compare_and_set(&provider, &key, None, &second, ACCOUNT_BINDING_TTL),
    );
    let winner = concurrent_winner(
        first_result.expect("first claim"),
        second_result.expect("second claim"),
    );
    assert!(winner.account_id() == &first || winner.account_id() == &second);
    assert_eq!(
        repository.load(&provider, &key).await.expect("load winner"),
        Some(winner)
    );
}

#[tokio::test]
async fn session_affinity_concurrent_switches_should_return_one_new_generation() {
    let Some((repository, _connection, _namespace)) = affinity_repository().await else {
        return;
    };
    let other_repository = repository.clone();
    let provider = ProviderKind::new("openai").expect("provider");
    let key = SessionAffinityKey::try_new("concurrent-switch").expect("affinity key");
    let original = ProviderAccountId::new("acct_original").expect("account");
    let first = ProviderAccountId::new("acct_first").expect("account");
    let second = ProviderAccountId::new("acct_second").expect("account");
    let observed = applied(
        repository
            .compare_and_set(&provider, &key, None, &original, ACCOUNT_BINDING_TTL)
            .await
            .expect("create original binding"),
    );

    let (first_result, second_result) = tokio::join!(
        repository.compare_and_set(
            &provider,
            &key,
            Some(&observed),
            &first,
            ACCOUNT_BINDING_TTL
        ),
        other_repository.compare_and_set(
            &provider,
            &key,
            Some(&observed),
            &second,
            ACCOUNT_BINDING_TTL,
        ),
    );
    let winner = concurrent_winner(
        first_result.expect("first switch"),
        second_result.expect("second switch"),
    );
    assert!(winner.account_id() == &first || winner.account_id() == &second);
    assert_ne!(winner.token(), observed.token());
    assert_eq!(
        repository.load(&provider, &key).await.expect("load winner"),
        Some(winner)
    );
}

#[tokio::test]
async fn session_affinity_stale_generation_should_not_switch_or_renew_after_returning_to_account() {
    let Some((repository, mut connection, namespace)) = affinity_repository().await else {
        return;
    };
    let provider = ProviderKind::new("openai").expect("provider");
    let key = SessionAffinityKey::try_new("aba-session").expect("affinity key");
    let first = ProviderAccountId::new("acct_first").expect("account");
    let second = ProviderAccountId::new("acct_second").expect("account");
    let original = applied(
        repository
            .compare_and_set(&provider, &key, None, &first, ACCOUNT_BINDING_TTL)
            .await
            .expect("create original binding"),
    );
    let migrated = applied(
        repository
            .compare_and_set(
                &provider,
                &key,
                Some(&original),
                &second,
                ACCOUNT_BINDING_TTL,
            )
            .await
            .expect("switch to second account"),
    );
    let current = applied(
        repository
            .compare_and_set(
                &provider,
                &key,
                Some(&migrated),
                &first,
                ACCOUNT_BINDING_TTL,
            )
            .await
            .expect("return to first account"),
    );
    assert_ne!(current.token(), original.token());
    let redis_key = only_namespace_key(&mut connection, &namespace).await;
    expire_in(&mut connection, &redis_key, 60_000).await;

    for selected in [&first, &second] {
        assert_eq!(
            repository
                .compare_and_set(
                    &provider,
                    &key,
                    Some(&original),
                    selected,
                    ACCOUNT_BINDING_TTL
                )
                .await
                .expect("reject stale generation"),
            AffinityUpdate::Conflict(Some(current.clone()))
        );
        assert!((1..=60_000).contains(&ttl_millis(&mut connection, &redis_key).await));
    }
    assert_eq!(
        repository
            .load(&provider, &key)
            .await
            .expect("load current"),
        Some(current)
    );
}

#[tokio::test]
async fn session_affinity_expired_binding_should_require_a_new_unbound_decision() {
    let Some((repository, mut connection, namespace)) = affinity_repository().await else {
        return;
    };
    let provider = ProviderKind::new("openai").expect("provider");
    let key = SessionAffinityKey::try_new("expired-session").expect("affinity key");
    let first = ProviderAccountId::new("acct_first").expect("account");
    let second = ProviderAccountId::new("acct_second").expect("account");
    let expired = applied(
        repository
            .compare_and_set(&provider, &key, None, &first, ACCOUNT_BINDING_TTL)
            .await
            .expect("create expiring binding"),
    );
    let redis_key = only_namespace_key(&mut connection, &namespace).await;
    expire_in(&mut connection, &redis_key, 0).await;

    for selected in [&first, &second] {
        assert_eq!(
            repository
                .compare_and_set(
                    &provider,
                    &key,
                    Some(&expired),
                    selected,
                    ACCOUNT_BINDING_TTL
                )
                .await
                .expect("reject expired observation"),
            AffinityUpdate::Conflict(None)
        );
    }
    assert_eq!(
        repository.load(&provider, &key).await.expect("load absent"),
        None
    );
    let recreated = applied(
        repository
            .compare_and_set(&provider, &key, None, &first, ACCOUNT_BINDING_TTL)
            .await
            .expect("bind from fresh absent observation"),
    );
    assert_ne!(recreated.token(), expired.token());
    assert_eq!(
        repository
            .compare_and_set(
                &provider,
                &key,
                Some(&expired),
                &second,
                ACCOUNT_BINDING_TTL
            )
            .await
            .expect("reject observation from before expiry"),
        AffinityUpdate::Conflict(Some(recreated))
    );
}

#[tokio::test]
async fn session_affinity_only_current_admission_should_renew_seven_day_ttl() {
    let Some((repository, mut connection, namespace)) = affinity_repository().await else {
        return;
    };
    let provider = ProviderKind::new("openai").expect("provider");
    let key = SessionAffinityKey::try_new("ttl-session").expect("affinity key");
    let account = ProviderAccountId::new("acct_ttl").expect("account");
    let current = applied(
        repository
            .compare_and_set(&provider, &key, None, &account, ACCOUNT_BINDING_TTL)
            .await
            .expect("create binding"),
    );
    let redis_key = only_namespace_key(&mut connection, &namespace).await;
    assert!((604_790_000..=604_800_000).contains(&ttl_millis(&mut connection, &redis_key).await));
    expire_in(&mut connection, &redis_key, 60_000).await;

    assert_eq!(
        repository
            .load(&provider, &key)
            .await
            .expect("read binding"),
        Some(current.clone())
    );
    assert!((1..=60_000).contains(&ttl_millis(&mut connection, &redis_key).await));
    assert_eq!(
        repository
            .compare_and_set(&provider, &key, None, &account, ACCOUNT_BINDING_TTL)
            .await
            .expect("reject stale absent observation"),
        AffinityUpdate::Conflict(Some(current.clone()))
    );
    assert!((1..=60_000).contains(&ttl_millis(&mut connection, &redis_key).await));

    // 格式和字段顺序不属于绑定代次；语义相同的存储记录仍可按原观察续期
    redis::cmd("SET")
        .arg(&redis_key)
        .arg(format!(
            "{{ \"token\": \"{}\", \"accountId\": \"{}\" }}",
            current.token().expose_to_store(),
            account.as_str(),
        ))
        .arg("PX")
        .arg(60_000)
        .query_async::<()>(&mut connection)
        .await
        .expect("store equivalent JSON representation");
    let renewed = applied(
        repository
            .compare_and_set(
                &provider,
                &key,
                Some(&current),
                &account,
                ACCOUNT_BINDING_TTL,
            )
            .await
            .expect("renew current binding"),
    );
    assert_eq!(renewed, current);
    assert!((604_790_000..=604_800_000).contains(&ttl_millis(&mut connection, &redis_key).await));
}

#[tokio::test]
async fn session_affinity_invalid_record_should_fail_closed_without_overwrite() {
    let Some((repository, mut connection, namespace)) = affinity_repository().await else {
        return;
    };
    let provider = ProviderKind::new("openai").expect("provider");
    let key = SessionAffinityKey::try_new("invalid-session").expect("affinity key");
    let account = ProviderAccountId::new("acct_first").expect("account");
    let observed = applied(
        repository
            .compare_and_set(&provider, &key, None, &account, ACCOUNT_BINDING_TTL)
            .await
            .expect("create binding before corrupting storage"),
    );
    let redis_key = only_namespace_key(&mut connection, &namespace).await;
    let token = observed.token().expose_to_store();
    let malformed_records = [
        "malformed-binding".to_owned(),
        serde_json::json!({"accountId": account.as_str()}).to_string(),
        serde_json::json!({"accountId": account.as_str(), "token": "invalid"}).to_string(),
        serde_json::json!({"accountId": "", "token": token}).to_string(),
        serde_json::json!({"accountId": account.as_str(), "token": token, "unexpected": true})
            .to_string(),
        format!(
            "{{\"accountId\":\"{}\",\"token\":\"{}\",\"token\":\"{}\"}}",
            account.as_str(),
            token,
            token,
        ),
    ];
    for malformed in malformed_records {
        redis::cmd("SET")
            .arg(&redis_key)
            .arg(&malformed)
            .arg("PX")
            .arg(60_000)
            .query_async::<()>(&mut connection)
            .await
            .expect("write malformed record");
        assert_eq!(
            repository
                .load(&provider, &key)
                .await
                .expect_err("reject malformed record")
                .kind(),
            AffinityStoreErrorKind::InvalidData
        );
        for expected in [None, Some(&observed)] {
            assert_eq!(
                repository
                    .compare_and_set(&provider, &key, expected, &account, ACCOUNT_BINDING_TTL)
                    .await
                    .expect_err("reject write over malformed record")
                    .kind(),
                AffinityStoreErrorKind::InvalidData
            );
        }
        assert_eq!(
            redis::cmd("GET")
                .arg(&redis_key)
                .query_async::<String>(&mut connection)
                .await
                .expect("read original malformed record"),
            malformed
        );
        assert!((1..=60_000).contains(&ttl_millis(&mut connection, &redis_key).await));
    }
}

#[tokio::test]
async fn session_affinity_wrong_redis_type_should_not_be_treated_as_absent() {
    let Some((repository, mut connection, namespace)) = affinity_repository().await else {
        return;
    };
    let provider = ProviderKind::new("openai").expect("provider");
    let key = SessionAffinityKey::try_new("wrong-type-session").expect("affinity key");
    let account = ProviderAccountId::new("acct_first").expect("account");
    applied(
        repository
            .compare_and_set(&provider, &key, None, &account, ACCOUNT_BINDING_TTL)
            .await
            .expect("create binding to locate key"),
    );
    let redis_key = only_namespace_key(&mut connection, &namespace).await;
    expire_in(&mut connection, &redis_key, 0).await;
    redis::cmd("RPUSH")
        .arg(&redis_key)
        .arg("unexpected-list-value")
        .query_async::<u64>(&mut connection)
        .await
        .expect("write wrong Redis type");

    assert!(repository.load(&provider, &key).await.is_err());
    assert!(
        repository
            .compare_and_set(&provider, &key, None, &account, ACCOUNT_BINDING_TTL)
            .await
            .is_err()
    );
    assert_eq!(
        redis::cmd("LRANGE")
            .arg(&redis_key)
            .arg(0)
            .arg(-1)
            .query_async::<Vec<String>>(&mut connection)
            .await
            .expect("retain wrong Redis type"),
        vec!["unexpected-list-value"]
    );
}

fn applied(update: AffinityUpdate) -> AccountBinding {
    match update {
        AffinityUpdate::Applied(binding) => binding,
        AffinityUpdate::Conflict(current) => panic!("expected applied binding, got {current:?}"),
    }
}

fn concurrent_winner(first: AffinityUpdate, second: AffinityUpdate) -> AccountBinding {
    match (first, second) {
        (AffinityUpdate::Applied(winner), AffinityUpdate::Conflict(Some(current)))
        | (AffinityUpdate::Conflict(Some(current)), AffinityUpdate::Applied(winner)) => {
            assert_eq!(winner, current);
            winner
        }
        results => panic!("expected one applied binding and one conflict, got {results:?}"),
    }
}

async fn namespace_keys(connection: &mut ConnectionManager, namespace: &str) -> Vec<String> {
    redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async(connection)
        .await
        .expect("list isolated affinity keys")
}

async fn only_namespace_key(connection: &mut ConnectionManager, namespace: &str) -> String {
    let mut keys = namespace_keys(connection, namespace).await;
    assert_eq!(keys.len(), 1);
    keys.pop().expect("binding key")
}

async fn expire_in(connection: &mut ConnectionManager, key: &str, ttl_millis: i64) {
    let changed = redis::cmd("PEXPIRE")
        .arg(key)
        .arg(ttl_millis)
        .query_async::<bool>(connection)
        .await
        .expect("set binding expiry");
    assert!(changed);
}

async fn ttl_millis(connection: &mut ConnectionManager, key: &str) -> i64 {
    redis::cmd("PTTL")
        .arg(key)
        .query_async(connection)
        .await
        .expect("read binding TTL")
}

async fn affinity_repository() -> Option<(
    RedisProviderSessionAffinityRepository,
    ConnectionManager,
    String,
)> {
    let redis_url = crate::support::test_env("CPR_TEST_REDIS_URL")?;
    let client = redis::Client::open(redis_url).expect("valid CPR_TEST_REDIS_URL");
    let connection = client
        .get_connection_manager()
        .await
        .expect("connect test Redis");
    let namespace = format!("gateway-store-affinity-test-{}", Uuid::new_v4());
    let repository = RedisProviderSessionAffinityRepository::new(connection.clone(), &namespace)
        .expect("valid test namespace");
    Some((repository, connection, namespace))
}
