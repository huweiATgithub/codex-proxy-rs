//! 账号辅助信息的持久观测、稳定身份隔离与时序保护

use std::collections::BTreeMap;

use gateway_admin::model::provider_credentials::{
    AccountInsightObservation, AccountInsightSnapshot, AccountInsights,
};

use super::*;

fn identity(account: &AccountRecord) -> serde_json::Value {
    serde_json::json!([
        account.provider_kind.as_str(),
        account.authentication_kind,
        account.upstream_user_id,
        account.upstream_account_id,
    ])
}

pub(super) async fn load(
    pool: &PgPool,
    accounts: &[AccountRecord],
) -> AdminStoreResult<BTreeMap<String, AccountInsights>> {
    if accounts.is_empty() {
        return Ok(BTreeMap::new());
    }
    let identities = accounts
        .iter()
        .map(|account| (account.id.as_str(), identity(account)))
        .collect::<BTreeMap<_, _>>();
    let ids = accounts
        .iter()
        .map(|account| account.id.as_str())
        .collect::<Vec<_>>();
    let rows = sqlx::query(
        "select account_id, identity_json, observed_at, observation_json
           from provider_account_insights where account_id = any($1)",
    )
    .bind(&ids)
    .fetch_all(pool)
    .await
    .map_err(|source| {
        admin_store_error(
            ENTITY,
            postgres_unavailable("load account insights", source),
        )
    })?;
    let mut insights = BTreeMap::<String, AccountInsights>::new();
    for row in rows {
        let account_id: String = row.try_get("account_id").map_err(|source| {
            admin_store_error(
                ENTITY,
                postgres_unavailable("decode account insight ID", source),
            )
        })?;
        let stored_identity: serde_json::Value =
            row.try_get("identity_json").map_err(|source| {
                admin_store_error(
                    ENTITY,
                    postgres_unavailable("decode account insight identity", source),
                )
            })?;
        if identities.get(account_id.as_str()) != Some(&stored_identity) {
            continue;
        }
        let observed_at = row.try_get("observed_at").map_err(|source| {
            admin_store_error(
                ENTITY,
                postgres_unavailable("decode account insight time", source),
            )
        })?;
        let observation: sqlx::types::Json<AccountInsightObservation> =
            row.try_get("observation_json").map_err(|source| {
                admin_store_error(
                    ENTITY,
                    postgres_unavailable("decode account insight", source),
                )
            })?;
        let target = insights.entry(account_id).or_default();
        match observation.0 {
            AccountInsightObservation::Subscription(value) => {
                target.subscription = Some(AccountInsightSnapshot { observed_at, value });
            }
            AccountInsightObservation::ResetCredits(value) => {
                target.reset_credits = Some(AccountInsightSnapshot { observed_at, value });
            }
        }
    }
    Ok(insights)
}

pub(super) async fn observe(
    pool: &PgPool,
    account: &AccountRecord,
    observed_at: DateTime<Utc>,
    observation: &AccountInsightObservation,
) -> AdminStoreResult<bool> {
    let kind = match observation {
        AccountInsightObservation::Subscription(_) => "subscription",
        AccountInsightObservation::ResetCredits(_) => "reset_credits",
    };
    // 锁住账号直到提交，保证换绑、删除与观测的身份检查在同一事务顺序内
    let affected = sqlx::query(
        "with current_account as (
             select id from provider_accounts
              where id = $1 and credential_revision = $2
                and jsonb_build_array(provider_kind, authentication_kind,
                      upstream_user_id, upstream_account_id) = $3
              for update
         )
         insert into provider_account_insights
             (account_id, kind, identity_json, observed_at, observation_json)
         select id, $4, $3, $5, $6 from current_account
         on conflict (account_id, kind) do update
             set identity_json = excluded.identity_json,
                 observed_at = excluded.observed_at,
                 observation_json = excluded.observation_json
         where provider_account_insights.identity_json <> excluded.identity_json
            or provider_account_insights.observed_at <= excluded.observed_at",
    )
    .bind(&account.id)
    .bind(
        to_i64(account.credential_revision.get())
            .map_err(|error| admin_store_error(ENTITY, error))?,
    )
    .bind(identity(account))
    .bind(kind)
    .bind(observed_at)
    .bind(sqlx::types::Json(observation))
    .execute(pool)
    .await
    .map_err(|source| {
        admin_store_error(
            ENTITY,
            postgres_unavailable("observe account insight", source),
        )
    })?;
    Ok(affected.rows_affected() == 1)
}
