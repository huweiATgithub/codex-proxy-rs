-- 自动使用重置卡的剩余周额度上限，空值表示不限制
ALTER TABLE runtime_settings
    ADD COLUMN reset_credit_max_weekly_remaining_percent integer
        CHECK (reset_credit_max_weekly_remaining_percent BETWEEN 0 AND 100);
