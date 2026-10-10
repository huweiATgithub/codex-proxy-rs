//! 辅助信息的迁移、持久性、身份与世代隔离、时序和孤儿清理测试

use gateway_admin::model::provider_credentials::{
    AccountInsightObservation, ProviderResetCredit, ProviderResetCredits, ProviderSubscription,
};

use super::*;

#[tokio::test]
async fn insights_should_survive_upgrade_and_reconnection_without_changing_config_revision() {
    let Some(database) = TestDatabase::create_through("insights_upgrade", 27).await else {
        return;
    };
    PgProviderAccountRepository::new(database.pool.clone())
        .insert_provider_account(account("acct_insights", "user-insights"))
        .await
        .unwrap();
    super::super::TEST_MIGRATOR
        .run(&database.pool)
        .await
        .unwrap();
    let store = admin_account_store(&database.pool);
    let detail = store
        .credential_details_by_id(&ProviderAccountId::new("acct_insights").unwrap())
        .await
        .unwrap()
        .unwrap();
    let record = &detail.credential;
    let now = Utc::now();
    let subscription = ProviderSubscription {
        starts_at: Some(now),
        expires_at: now + TimeDelta::days(30),
        will_renew: Some(true),
        billing_period: Some("monthly".into()),
        billing_currency: Some("USD".into()),
        observed_at: now,
    };
    assert!(
        store
            .observe_account_insight(
                record,
                now,
                &AccountInsightObservation::Subscription(Some(subscription.clone()))
            )
            .await
            .unwrap()
    );
    let credits = ProviderResetCredits {
        observed_at: now,
        available_count: 2,
        credits: vec![ProviderResetCredit {
            id: "credit-expiry".into(),
            status: Some("available".into()),
            title: Some("用量重置".into()),
            expires_at: Some(now + TimeDelta::days(7)),
            reset_type: Some("weekly".into()),
        }],
    };
    assert!(
        store
            .observe_account_insight(
                record,
                now,
                &AccountInsightObservation::ResetCredits(Some(credits.clone()))
            )
            .await
            .unwrap()
    );
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_with((*database.pool.connect_options()).clone())
        .await
        .unwrap();
    let reconnected = admin_account_store(&pool);
    let saved = reconnected
        .load_account_insights(std::slice::from_ref(record))
        .await
        .unwrap()
        .remove(&record.id)
        .unwrap();
    assert_eq!(saved.subscription.unwrap().value, Some(subscription));
    assert_eq!(saved.reset_credits.unwrap().value, Some(credits.clone()));
    assert_eq!(
        reconnected
            .credential_details_by_id(&ProviderAccountId::new(record.id.clone()).unwrap())
            .await
            .unwrap()
            .unwrap()
            .config_revision,
        detail.config_revision
    );

    assert!(
        !store
            .observe_account_insight(
                record,
                now - TimeDelta::seconds(1),
                &AccountInsightObservation::ResetCredits(Some(credits.clone()))
            )
            .await
            .unwrap()
    );
    let invalidated_at = now + TimeDelta::seconds(1);
    assert!(
        store
            .observe_account_insight(
                record,
                invalidated_at,
                &AccountInsightObservation::ResetCredits(None)
            )
            .await
            .unwrap()
    );
    assert!(
        !store
            .observe_account_insight(
                record,
                now,
                &AccountInsightObservation::ResetCredits(Some(credits))
            )
            .await
            .unwrap()
    );
    let cleared = reconnected
        .load_account_insights(std::slice::from_ref(record))
        .await
        .unwrap()
        .remove(&record.id)
        .unwrap();
    assert!(cleared.reset_credits.unwrap().value.is_none());
    pool.close().await;
    database.close().await;
}

#[tokio::test]
async fn insights_should_survive_token_rotation_but_be_hidden_after_rebinding_and_pruned_after_deletion()
 {
    let Some(database) = TestDatabase::create("insights_identity").await else {
        return;
    };
    PgProviderAccountRepository::new(database.pool.clone())
        .insert_provider_account(account("acct_insights", "user-insights"))
        .await
        .unwrap();
    let store = admin_account_store(&database.pool);
    let id = ProviderAccountId::new("acct_insights").unwrap();
    let original = store
        .credential_details_by_id(&id)
        .await
        .unwrap()
        .unwrap()
        .credential;
    let now = Utc::now();
    let observation = AccountInsightObservation::ResetCredits(Some(ProviderResetCredits {
        observed_at: now,
        available_count: 1,
        credits: Vec::new(),
    }));
    assert!(
        store
            .observe_account_insight(&original, now, &observation)
            .await
            .unwrap()
    );
    sqlx::query(
        "update provider_accounts set credential_revision = credential_revision + 1 where id = $1",
    )
    .bind(id.as_str())
    .execute(&database.pool)
    .await
    .unwrap();
    let rotated = store
        .credential_details_by_id(&id)
        .await
        .unwrap()
        .unwrap()
        .credential;
    assert_eq!(
        store
            .load_account_insights(std::slice::from_ref(&rotated))
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        !store
            .observe_account_insight(&original, now + TimeDelta::seconds(1), &observation)
            .await
            .unwrap()
    );
    sqlx::query("update provider_accounts set upstream_user_id = 'user-rebound', credential_revision = credential_revision + 1 where id = $1").bind(id.as_str()).execute(&database.pool).await.unwrap();
    let rebound = store
        .credential_details_by_id(&id)
        .await
        .unwrap()
        .unwrap()
        .credential;
    assert!(
        store
            .load_account_insights(std::slice::from_ref(&rebound))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        !store
            .observe_account_insight(&rotated, now + TimeDelta::seconds(2), &observation)
            .await
            .unwrap()
    );
    assert!(
        store
            .observe_account_insight(&rebound, now + TimeDelta::seconds(3), &observation)
            .await
            .unwrap()
    );
    sqlx::query("delete from provider_accounts where id = $1")
        .bind(id.as_str())
        .execute(&database.pool)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("select count(*) from fork_account_insights")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert!(
        !store
            .observe_account_insight(&rebound, now + TimeDelta::seconds(4), &observation)
            .await
            .unwrap()
    );
    store.prune_orphan_account_insights().await.unwrap();
    let count: i64 = sqlx::query_scalar("select count(*) from fork_account_insights")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    database.close().await;
}

#[tokio::test]
async fn insights_should_survive_upstream_migrations_applied_after_an_existing_fork_migration() {
    let Some(database) = TestDatabase::create_through("insights_fork_upgrade", 27).await else {
        return;
    };
    let old_fork = sqlx::migrate::Migrator::with_migrations(
        super::super::TEST_MIGRATOR
            .iter()
            .filter(|migration| migration.version <= 27 || migration.version == 9001)
            .cloned()
            .collect(),
    );
    old_fork.run(&database.pool).await.unwrap();
    PgProviderAccountRepository::new(database.pool.clone())
        .insert_provider_account(account("acct_insights", "user-insights"))
        .await
        .unwrap();
    let store = admin_account_store(&database.pool);
    let record = store
        .credential_details_by_id(&ProviderAccountId::new("acct_insights").unwrap())
        .await
        .unwrap()
        .unwrap()
        .credential;
    let observed_at = Utc::now();
    let observation = AccountInsightObservation::ResetCredits(Some(ProviderResetCredits {
        observed_at,
        available_count: 3,
        credits: Vec::new(),
    }));
    assert!(
        store
            .observe_account_insight(&record, observed_at, &observation)
            .await
            .unwrap()
    );
    let fork_ledger_before: (chrono::DateTime<Utc>, Vec<u8>, bool) = sqlx::query_as(
        "select installed_on, checksum, success from _sqlx_migrations where version = 9001",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    let old_versions: Vec<i64> =
        sqlx::query_scalar("select version from _sqlx_migrations order by version")
            .fetch_all(&database.pool)
            .await
            .unwrap();
    assert_eq!(old_versions, (1..=27).chain([9001]).collect::<Vec<_>>());

    super::super::TEST_MIGRATOR
        .run(&database.pool)
        .await
        .unwrap();
    let fork_ledger_after: (chrono::DateTime<Utc>, Vec<u8>, bool) = sqlx::query_as(
        "select installed_on, checksum, success from _sqlx_migrations where version = 9001",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(fork_ledger_after, fork_ledger_before);
    let versions: Vec<i64> =
        sqlx::query_scalar("select version from _sqlx_migrations where success order by version")
            .fetch_all(&database.pool)
            .await
            .unwrap();
    assert_eq!(versions, (1..=29).chain([9001]).collect::<Vec<_>>());
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_with((*database.pool.connect_options()).clone())
        .await
        .unwrap();
    super::super::TEST_MIGRATOR.run(&pool).await.unwrap();
    let saved = admin_account_store(&pool)
        .load_account_insights(std::slice::from_ref(&record))
        .await
        .unwrap()
        .remove(&record.id)
        .unwrap();
    assert_eq!(
        saved.reset_credits.unwrap().value.unwrap().available_count,
        3
    );
    pool.close().await;
    database.close().await;
}

#[tokio::test]
async fn insights_should_not_cross_account_recreation_even_with_the_same_identity_and_revision() {
    let Some(database) = TestDatabase::create("insights_recreation").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    repository
        .insert_provider_account(account("acct_insights", "user-insights"))
        .await
        .unwrap();
    let store = admin_account_store(&database.pool);
    let id = ProviderAccountId::new("acct_insights").unwrap();
    let original = store
        .credential_details_by_id(&id)
        .await
        .unwrap()
        .unwrap()
        .credential;
    let now = Utc::now();
    let observation = AccountInsightObservation::Subscription(None);
    assert!(
        store
            .observe_account_insight(&original, now, &observation)
            .await
            .unwrap()
    );
    sqlx::query("delete from provider_accounts where id = $1")
        .bind(id.as_str())
        .execute(&database.pool)
        .await
        .unwrap();
    repository
        .insert_provider_account(account("acct_insights", "user-insights"))
        .await
        .unwrap();
    sqlx::query(
        "update provider_accounts set created_at = $2, updated_at = greatest(updated_at, $2)
          where id = $1",
    )
    .bind(id.as_str())
    .bind(original.created_at + TimeDelta::seconds(1))
    .execute(&database.pool)
    .await
    .unwrap();
    let recreated = store
        .credential_details_by_id(&id)
        .await
        .unwrap()
        .unwrap()
        .credential;
    assert_eq!(recreated.credential_revision, original.credential_revision);
    assert_eq!(recreated.upstream_user_id, original.upstream_user_id);
    assert_ne!(recreated.created_at, original.created_at);
    assert!(
        store
            .load_account_insights(std::slice::from_ref(&recreated))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        !store
            .observe_account_insight(&original, now + TimeDelta::seconds(1), &observation)
            .await
            .unwrap()
    );
    // 新账号的观测时间即使更早，也不能被旧世代的时序保护阻挡
    assert!(
        store
            .observe_account_insight(&recreated, now - TimeDelta::seconds(1), &observation)
            .await
            .unwrap()
    );
    assert!(
        store
            .load_account_insights(std::slice::from_ref(&original))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .load_account_insights(std::slice::from_ref(&recreated))
            .await
            .unwrap()
            .len(),
        1
    );
    database.close().await;
}

#[tokio::test]
async fn pruning_should_remove_orphans_and_old_generations_but_preserve_current_snapshots() {
    let Some(database) = TestDatabase::create("insights_pruning").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let store = admin_account_store(&database.pool);
    let now = Utc::now();
    let observation = AccountInsightObservation::Subscription(None);
    for account_id in ["acct_live", "acct_deleted", "acct_recreated"] {
        repository
            .insert_provider_account(account(account_id, account_id))
            .await
            .unwrap();
        let record = store
            .credential_details_by_id(&ProviderAccountId::new(account_id).unwrap())
            .await
            .unwrap()
            .unwrap()
            .credential;
        assert!(
            store
                .observe_account_insight(&record, now, &observation)
                .await
                .unwrap()
        );
    }
    sqlx::query("delete from provider_accounts where id = 'acct_deleted'")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query(
        "update provider_accounts
            set created_at = created_at + interval '1 second',
                updated_at = greatest(updated_at, created_at + interval '1 second')
          where id = 'acct_recreated'",
    )
    .execute(&database.pool)
    .await
    .unwrap();
    store.prune_orphan_account_insights().await.unwrap();
    let retained: Vec<String> =
        sqlx::query_scalar("select account_id from fork_account_insights order by account_id")
            .fetch_all(&database.pool)
            .await
            .unwrap();
    assert_eq!(retained, ["acct_live"]);
    store.prune_orphan_account_insights().await.unwrap();
    let retained_again: Vec<String> =
        sqlx::query_scalar("select account_id from fork_account_insights order by account_id")
            .fetch_all(&database.pool)
            .await
            .unwrap();
    assert_eq!(retained_again, retained);
    database.close().await;
}

#[tokio::test]
async fn pruning_should_preserve_a_fresh_generation_written_while_waiting_for_the_snapshot_lock() {
    use sqlx::Connection as _;

    let Some(database) = TestDatabase::create("insights_prune_race").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    repository
        .insert_provider_account(account("acct_insights", "user-insights"))
        .await
        .unwrap();
    let store = admin_account_store(&database.pool);
    let id = ProviderAccountId::new("acct_insights").unwrap();
    let original = store
        .credential_details_by_id(&id)
        .await
        .unwrap()
        .unwrap()
        .credential;
    let now = Utc::now();
    assert!(
        store
            .observe_account_insight(
                &original,
                now,
                &AccountInsightObservation::ResetCredits(None)
            )
            .await
            .unwrap()
    );
    sqlx::query("delete from provider_accounts where id = $1")
        .bind(id.as_str())
        .execute(&database.pool)
        .await
        .unwrap();
    repository
        .insert_provider_account(account("acct_insights", "user-insights"))
        .await
        .unwrap();
    sqlx::query(
        "update provider_accounts set created_at = $2, updated_at = greatest(updated_at, $2)
          where id = $1",
    )
    .bind(id.as_str())
    .bind(original.created_at + TimeDelta::seconds(1))
    .execute(&database.pool)
    .await
    .unwrap();
    let recreated = store
        .credential_details_by_id(&id)
        .await
        .unwrap()
        .unwrap()
        .credential;
    let fresh = AccountInsightObservation::ResetCredits(Some(ProviderResetCredits {
        observed_at: now,
        available_count: 4,
        credits: Vec::new(),
    }));
    let mut writer = sqlx::PgConnection::connect_with(&database.pool.connect_options())
        .await
        .unwrap();
    let writer_pid: i32 = sqlx::query_scalar("select pg_backend_pid()")
        .fetch_one(&mut writer)
        .await
        .unwrap();
    let mut transaction = writer.begin().await.unwrap();
    sqlx::query(
        "update fork_account_insights
            set account_created_at = $2, observed_at = $3, observation_json = $4
          where account_id = $1",
    )
    .bind(id.as_str())
    .bind(recreated.created_at)
    .bind(now)
    .bind(sqlx::types::Json(&fresh))
    .execute(&mut *transaction)
    .await
    .unwrap();
    let cleaner = store.clone();
    let running = tokio::spawn(async move { cleaner.prune_orphan_account_insights().await });
    // 确认清理实际读到旧世代并等待写事务，避免靠客户端延时猜测并发顺序
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "select exists (
                     select 1 from pg_stat_activity
                      where wait_event_type = 'Lock' and $1 = any(pg_blocking_pids(pid))
                        and query like 'with orphans as materialized%'
                 )",
            )
            .bind(writer_pid)
            .fetch_one(&database.pool)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("pruning must wait for the snapshot writer");
    transaction.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    store.prune_orphan_account_insights().await.unwrap();
    let saved = store
        .load_account_insights(std::slice::from_ref(&recreated))
        .await
        .unwrap()
        .remove(&recreated.id)
        .unwrap();
    assert_eq!(
        saved.reset_credits.unwrap().value.unwrap().available_count,
        4
    );
    writer.close().await.unwrap();
    database.close().await;
}
