-- 账号订阅与主动重置卡的最近观测，按稳定身份隔离，独立于 credential 轮换
create table provider_account_insights (
    account_id text not null references provider_accounts(id) on delete cascade,
    kind text not null check (kind in ('subscription', 'reset_credits')),
    identity_json jsonb not null,
    observed_at timestamptz not null,
    observation_json jsonb not null,
    primary key (account_id, kind)
);
