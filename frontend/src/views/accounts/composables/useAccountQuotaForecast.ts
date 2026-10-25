import type { Ref } from 'vue'
import type { Account } from '@/api'
import { computed, nextTick, onScopeDispose, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { refreshAccountQuota } from '@/api'
import { ApiError } from '@/api/request'
import { notification as ZNotification } from '@/utils/notification'
import { errorMessage } from '@/utils/operation'
import { useAccountInsights } from './useAccountInsights'

export function useAccountQuotaForecast(
  account: Ref<Account>,
  open: Ref<boolean>,
  onAccountUpdated?: (account: Account) => void,
) {
  const { t } = useI18n({ useScope: 'global' })
  const insights = useAccountInsights()
  const state = computed(() => insights.forecast(account.value))
  const report = computed(() => state.value.report)
  const loading = computed(() => state.value.loading)
  const error = computed(() => state.value.error)
  const refreshing = shallowRef(false)
  let disposed = false

  function load() {
    if (!open.value || disposed || account.value.authenticationKind === 'api_key')
      return Promise.resolve(false)
    return insights.loadForecast(account.value)
  }

  async function refresh() {
    if (refreshing.value || !open.value)
      return
    const targetAccountId = account.value.id
    insights.invalidateForecast(account.value)
    refreshing.value = true
    try {
      // 刷新仍由用户显式触发，列表与弹窗只共享本地预测结果
      const result = await refreshAccountQuota({ accountId: targetAccountId })
      if (disposed)
        return
      onAccountUpdated?.(result.account)
      await nextTick()
      if (account.value.id === targetAccountId && await load())
        ZNotification.success({ message: t('accounts.actions.quotaRefreshed') })
    }
    catch (cause) {
      if (!disposed && account.value.id === targetAccountId && !report.value
        && !(cause instanceof ApiError && cause.kind === 'cancelled')) {
        state.value.error = errorMessage(cause)
      }
    }
    finally {
      refreshing.value = false
    }
  }

  watch([open, account], ([isOpen]) => {
    if (isOpen && !report.value)
      void load()
  }, { immediate: true })

  onScopeDispose(() => {
    disposed = true
  })

  return { report, loading, refreshing, error, load, refresh }
}
