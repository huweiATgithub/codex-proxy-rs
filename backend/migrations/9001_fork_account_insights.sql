-- 账号订阅与主动重置卡的最近观测，按账号创建时间和稳定身份隔离
create table fork_account_insights (
    account_id text not null,
    account_created_at timestamptz not null,
    kind text not null check (kind in ('subscription', 'reset_credits')),
    identity_json jsonb not null,
    observed_at timestamptz not null,
    observation_json jsonb not null,
    primary key (account_id, kind)
);
