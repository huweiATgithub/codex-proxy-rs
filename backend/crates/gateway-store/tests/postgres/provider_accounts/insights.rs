//! 辅助信息的迁移、重连持久性、时序保护与账号身份隔离测试

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
async fn insights_should_survive_token_rotation_but_be_hidden_after_rebinding_and_deleted_with_account()
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
    let count: i64 = sqlx::query_scalar("select count(*) from provider_account_insights")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    database.close().await;
}
