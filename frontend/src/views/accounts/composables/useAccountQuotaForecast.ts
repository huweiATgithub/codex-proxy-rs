import type { Ref } from 'vue'
import type { Account } from '@/api'
import { ZNotification } from '@codex-proxy/ui'
import { computed, nextTick, onScopeDispose, shallowRef, watch } from 'vue'
import { refreshAccountQuota } from '@/api'
import { useAccountInsights } from './useAccountInsights'

export function useAccountQuotaForecast(
  account: Ref<Account>,
  open: Ref<boolean>,
  onAccountUpdated?: (account: Account) => void,
) {
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
    refreshing.value = true
    try {
      // 刷新仍由用户显式触发，列表与弹窗只共享本地预测结果
      const result = await refreshAccountQuota({ accountId: targetAccountId })
      if (disposed)
        return
      onAccountUpdated?.(result.account)
      await nextTick()
      if (account.value.id === targetAccountId && await load())
        ZNotification.success({ message: '额度已刷新' })
    }
    catch {
      if (!disposed && account.value.id === targetAccountId)
        state.value.error = true
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
