//! 验证会话绑定的并发认领、版本比较、过期和存储损坏边界

use std::time::Duration;

use gateway_core::account::ProviderAccountId;
use gateway_core::provider_ports::{
    ProviderSessionAffinityKey, ProviderSessionAffinityPort, ProviderSessionAlias,
    ProviderStoreErrorKind,
};
use gateway_core::routing::ProviderKind;
use gateway_store::redis::RedisProviderSessionAffinityRepository;
use redis::aio::ConnectionManager;
use uuid::Uuid;

#[tokio::test]
async fn concurrent_claims_admit_exactly_one_account() {
    let Some((repo, mut connection, namespace)) = affinity_repository().await else {
        return;
    };
    let provider = ProviderKind::new("openai").unwrap();
    let key = ProviderSessionAffinityKey::try_new("secret-session").unwrap();
    let first = ProviderAccountId::new("acct_first").unwrap();
    let second = ProviderAccountId::new("acct_second").unwrap();
    let (a, b) = tokio::join!(
        repo.compare_and_bind(&provider, &key, None, &first, Duration::from_secs(60)),
        repo.compare_and_bind(&provider, &key, None, &second, Duration::from_secs(60))
    );
    let winners = [a.unwrap(), b.unwrap()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(winners.len(), 1);
    assert_eq!(
        repo.load(&provider, &key).await.unwrap().as_ref(),
        winners.first()
    );
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async(&mut connection)
        .await
        .unwrap();
    assert_eq!(keys.len(), 1);
    assert!(!keys[0].contains("secret-session"));
    let ttl: i64 = redis::cmd("PTTL")
        .arg(&keys[0])
        .query_async(&mut connection)
        .await
        .unwrap();
    assert!((1..=60_000).contains(&ttl));
}

#[tokio::test]
async fn stale_revision_cannot_overwrite_an_account_that_returned_to_the_session() {
    let Some((repo, _, _)) = affinity_repository().await else {
        return;
    };
    let provider = ProviderKind::new("openai").unwrap();
    let key = ProviderSessionAffinityKey::try_new("aba-session").unwrap();
    let a = ProviderAccountId::new("acct_a").unwrap();
    let b = ProviderAccountId::new("acct_b").unwrap();
    let ttl = Duration::from_secs(60);
    let first = repo
        .compare_and_bind(&provider, &key, None, &a, ttl)
        .await
        .unwrap()
        .unwrap();
    let second = repo
        .compare_and_bind(&provider, &key, Some(&first), &b, ttl)
        .await
        .unwrap()
        .unwrap();
    let third = repo
        .compare_and_bind(&provider, &key, Some(&second), &a, ttl)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(first.revision(), third.revision());
    assert!(
        repo.compare_and_bind(&provider, &key, Some(&first), &b, ttl)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(repo.load(&provider, &key).await.unwrap(), Some(third));
}

#[tokio::test]
async fn renewal_preserves_revision_but_expiration_requires_fresh_claim() {
    let Some((repo, mut connection, namespace)) = affinity_repository().await else {
        return;
    };
    let provider = ProviderKind::new("openai").unwrap();
    let key = ProviderSessionAffinityKey::try_new("expired-session").unwrap();
    let a = ProviderAccountId::new("acct_a").unwrap();
    let ttl = Duration::from_secs(60);
    let first = repo
        .compare_and_bind(&provider, &key, None, &a, ttl)
        .await
        .unwrap()
        .unwrap();
    let renewed = repo
        .compare_and_bind(&provider, &key, Some(&first), &a, ttl)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first, renewed);
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async(&mut connection)
        .await
        .unwrap();
    let _: bool = redis::cmd("PEXPIRE")
        .arg(&keys[0])
        .arg(0)
        .query_async(&mut connection)
        .await
        .unwrap();
    assert!(
        repo.compare_and_bind(&provider, &key, Some(&first), &a, ttl)
            .await
            .unwrap()
            .is_none()
    );
    assert!(repo.load(&provider, &key).await.unwrap().is_none());
    let fresh = repo
        .compare_and_bind(&provider, &key, None, &a, ttl)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(first.revision(), fresh.revision());
}

#[tokio::test]
async fn damaged_binding_is_an_error_and_cannot_be_claimed_as_absent() {
    let Some((repo, mut connection, namespace)) = affinity_repository().await else {
        return;
    };
    let provider = ProviderKind::new("openai").unwrap();
    let key = ProviderSessionAffinityKey::try_new("damaged-session").unwrap();
    let a = ProviderAccountId::new("acct_a").unwrap();
    let ttl = Duration::from_secs(60);
    repo.compare_and_bind(&provider, &key, None, &a, ttl)
        .await
        .unwrap()
        .unwrap();
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async(&mut connection)
        .await
        .unwrap();
    let _: () = redis::cmd("SET")
        .arg(&keys[0])
        .arg("invalid-binding")
        .query_async(&mut connection)
        .await
        .unwrap();
    assert!(repo.load(&provider, &key).await.is_err());
    assert!(
        repo.compare_and_bind(&provider, &key, None, &a, ttl)
            .await
            .unwrap()
            .is_none()
    );
}

async fn affinity_repository() -> Option<(
    RedisProviderSessionAffinityRepository,
    ConnectionManager,
    String,
)> {
    let redis_url = crate::support::test_env("CPR_TEST_REDIS_URL")?;
    let connection = redis::Client::open(redis_url)
        .unwrap()
        .get_connection_manager()
        .await
        .unwrap();
    let namespace = format!("gateway-store-affinity-test-{}", Uuid::new_v4());
    let repository =
        RedisProviderSessionAffinityRepository::new(connection.clone(), &namespace).unwrap();
    Some((repository, connection, namespace))
}

#[tokio::test]
async fn turn_alias_cannot_be_reassigned_and_follows_session_migration() {
    let Some((repo, _, _)) = affinity_repository().await else {
        return;
    };
    let provider = ProviderKind::new("openai").unwrap();
    let turn = ProviderSessionAffinityKey::try_new("client-turn").unwrap();
    let session = ProviderSessionAffinityKey::try_new("client-session").unwrap();
    let other = ProviderSessionAffinityKey::try_new("other-session").unwrap();
    let a = ProviderAccountId::new("acct_a").unwrap();
    let b = ProviderAccountId::new("acct_b").unwrap();
    let ttl = Duration::from_secs(60);
    let alias = gateway_core::provider_ports::ProviderSessionAlias {
        session_key: session.clone(),
        follow_only: true,
    };
    let other_alias = gateway_core::provider_ports::ProviderSessionAlias {
        session_key: other,
        follow_only: true,
    };
    assert!(
        repo.bind_alias(&provider, &turn, &alias, ttl)
            .await
            .unwrap()
    );
    assert!(
        !repo
            .bind_alias(&provider, &turn, &other_alias, ttl)
            .await
            .unwrap()
    );
    let different_role = gateway_core::provider_ports::ProviderSessionAlias {
        session_key: session.clone(),
        follow_only: false,
    };
    assert!(
        !repo
            .bind_alias(&provider, &turn, &different_role, ttl)
            .await
            .unwrap()
    );
    let first = repo
        .compare_and_bind(&provider, &session, None, &a, ttl)
        .await
        .unwrap()
        .unwrap();
    repo.compare_and_bind(&provider, &session, Some(&first), &b, ttl)
        .await
        .unwrap()
        .unwrap();
    let target = repo.load_alias(&provider, &turn).await.unwrap().unwrap();
    assert!(target.follow_only);
    assert_eq!(
        repo.load(&provider, &target.session_key)
            .await
            .unwrap()
            .unwrap()
            .account_id(),
        &b
    );
}

#[tokio::test]
async fn seven_day_binding_and_turn_alias_renew_after_admission_but_not_lookup() {
    let Some((repo, mut connection, namespace)) = affinity_repository().await else {
        return;
    };
    let provider = ProviderKind::new("openai").unwrap();
    let session = ProviderSessionAffinityKey::try_new("long-session").unwrap();
    let turn = ProviderSessionAffinityKey::try_new("long-turn").unwrap();
    let account = ProviderAccountId::new("acct_long").unwrap();
    let alias = ProviderSessionAlias {
        session_key: session.clone(),
        follow_only: true,
    };
    let ttl = Duration::from_secs(7 * 24 * 60 * 60);
    let first = repo
        .compare_and_bind(&provider, &session, None, &account, ttl)
        .await
        .unwrap()
        .unwrap();
    assert!(
        repo.bind_alias(&provider, &turn, &alias, ttl)
            .await
            .unwrap()
    );
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async(&mut connection)
        .await
        .unwrap();
    assert_eq!(keys.len(), 2);
    for key in &keys {
        let stored_ttl: i64 = redis::cmd("PTTL")
            .arg(key)
            .query_async(&mut connection)
            .await
            .unwrap();
        assert!(
            (604_790_000..=604_800_000).contains(&stored_ttl),
            "seven-day TTL was not retained: {stored_ttl}"
        );
        let _: bool = redis::cmd("PEXPIRE")
            .arg(key)
            .arg(60_000)
            .query_async(&mut connection)
            .await
            .unwrap();
    }
    assert_eq!(
        repo.load(&provider, &session).await.unwrap(),
        Some(first.clone())
    );
    assert_eq!(
        repo.load_alias(&provider, &turn).await.unwrap(),
        Some(alias.clone())
    );
    for key in &keys {
        let stored_ttl: i64 = redis::cmd("PTTL")
            .arg(key)
            .query_async(&mut connection)
            .await
            .unwrap();
        assert!(
            (1..=60_000).contains(&stored_ttl),
            "lookup unexpectedly renewed TTL: {stored_ttl}"
        );
    }
    let renewed = repo
        .compare_and_bind(&provider, &session, Some(&first), &account, ttl)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(renewed, first);
    assert!(
        repo.bind_alias(&provider, &turn, &alias, ttl)
            .await
            .unwrap()
    );
    for key in &keys {
        let stored_ttl: i64 = redis::cmd("PTTL")
            .arg(key)
            .query_async(&mut connection)
            .await
            .unwrap();
        assert!(
            (604_790_000..=604_800_000).contains(&stored_ttl),
            "admission did not renew TTL: {stored_ttl}"
        );
    }
}

#[tokio::test]
async fn binding_and_alias_conflicts_do_not_extend_ttl() {
    let Some((repo, mut connection, namespace)) = affinity_repository().await else {
        return;
    };
    let provider = ProviderKind::new("openai").unwrap();
    let session = ProviderSessionAffinityKey::try_new("conflict-session").unwrap();
    let turn = ProviderSessionAffinityKey::try_new("conflict-turn").unwrap();
    let first_account = ProviderAccountId::new("acct_first").unwrap();
    let next_account = ProviderAccountId::new("acct_next").unwrap();
    let short_ttl = Duration::from_secs(60);
    let long_ttl = Duration::from_secs(7 * 24 * 60 * 60);
    let first = repo
        .compare_and_bind(&provider, &session, None, &first_account, short_ttl)
        .await
        .unwrap()
        .unwrap();
    let next = repo
        .compare_and_bind(&provider, &session, Some(&first), &next_account, short_ttl)
        .await
        .unwrap()
        .unwrap();
    let alias = ProviderSessionAlias {
        session_key: session.clone(),
        follow_only: true,
    };
    assert!(
        repo.bind_alias(&provider, &turn, &alias, short_ttl)
            .await
            .unwrap()
    );
    assert!(
        repo.compare_and_bind(&provider, &session, Some(&first), &first_account, long_ttl)
            .await
            .unwrap()
            .is_none()
    );
    let conflicting = ProviderSessionAlias {
        session_key: session.clone(),
        follow_only: false,
    };
    assert!(
        !repo
            .bind_alias(&provider, &turn, &conflicting, long_ttl)
            .await
            .unwrap()
    );
    assert_eq!(repo.load(&provider, &session).await.unwrap(), Some(next));
    assert_eq!(
        repo.load_alias(&provider, &turn).await.unwrap(),
        Some(alias)
    );
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async(&mut connection)
        .await
        .unwrap();
    assert_eq!(keys.len(), 2);
    for key in keys {
        let stored_ttl: i64 = redis::cmd("PTTL")
            .arg(key)
            .query_async(&mut connection)
            .await
            .unwrap();
        assert!(
            (1..=60_000).contains(&stored_ttl),
            "conflict unexpectedly renewed TTL: {stored_ttl}"
        );
    }
}

#[tokio::test]
async fn binding_and_alias_reject_ttls_that_redis_cannot_express() {
    let Some((repo, mut connection, namespace)) = affinity_repository().await else {
        return;
    };
    let provider = ProviderKind::new("openai").unwrap();
    let session = ProviderSessionAffinityKey::try_new("invalid-ttl-session").unwrap();
    let turn = ProviderSessionAffinityKey::try_new("invalid-ttl-turn").unwrap();
    let account = ProviderAccountId::new("acct_ttl").unwrap();
    let alias = ProviderSessionAlias {
        session_key: session.clone(),
        follow_only: false,
    };
    for ttl in [Duration::ZERO, Duration::from_nanos(1), Duration::MAX] {
        let error = repo
            .compare_and_bind(&provider, &session, None, &account, ttl)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ProviderStoreErrorKind::InvalidData);
        let error = repo
            .bind_alias(&provider, &turn, &alias, ttl)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ProviderStoreErrorKind::InvalidData);
    }
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async(&mut connection)
        .await
        .unwrap();
    assert!(keys.is_empty());
}

#[tokio::test]
async fn damaged_turn_alias_is_an_error_and_cannot_be_replaced_as_absent() {
    let Some((repo, mut connection, namespace)) = affinity_repository().await else {
        return;
    };
    let provider = ProviderKind::new("openai").unwrap();
    let session = ProviderSessionAffinityKey::try_new("damaged-alias-session").unwrap();
    let turn = ProviderSessionAffinityKey::try_new("damaged-alias-turn").unwrap();
    let alias = ProviderSessionAlias {
        session_key: session,
        follow_only: false,
    };
    let ttl = Duration::from_secs(7 * 24 * 60 * 60);
    assert!(
        repo.bind_alias(&provider, &turn, &alias, ttl)
            .await
            .unwrap()
    );
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async(&mut connection)
        .await
        .unwrap();
    assert_eq!(keys.len(), 1);
    let _: () = redis::cmd("SET")
        .arg(&keys[0])
        .arg("damaged-alias")
        .arg("PX")
        .arg(60_000)
        .query_async(&mut connection)
        .await
        .unwrap();
    assert_eq!(
        repo.load_alias(&provider, &turn).await.unwrap_err().kind(),
        ProviderStoreErrorKind::InvalidData
    );
    assert!(
        !repo
            .bind_alias(&provider, &turn, &alias, ttl)
            .await
            .unwrap()
    );
    let stored_ttl: i64 = redis::cmd("PTTL")
        .arg(&keys[0])
        .query_async(&mut connection)
        .await
        .unwrap();
    assert!((1..=60_000).contains(&stored_ttl));
}
