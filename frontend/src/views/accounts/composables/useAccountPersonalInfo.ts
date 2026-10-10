import type { Ref } from 'vue'
import type { Account } from '@/api'
import { computed, watch } from 'vue'

import { accountInsightKey, useAccountInsights } from './useAccountInsights'

export function useAccountPersonalInfo({ account, open }: {
  account: Ref<Account>
  open: Ref<boolean>
}) {
  const insights = useAccountInsights()
  const key = computed(() => accountInsightKey(account.value))
  const state = computed(() => insights.personalInfo(account.value))
  const info = computed(() => state.value.info)
  const loading = computed(() => state.value.loading)
  const profile = computed(() => account.value.capabilities.profile ? info.value?.profile ?? null : null)
  const subscription = computed(() => account.value.capabilities.subscription ? info.value?.subscription ?? null : null)
  const error = computed(() => state.value.error || (account.value.capabilities.profile ? info.value?.profileError : '') || '')

  function load() {
    if (!open.value)
      return Promise.resolve(false)
    return insights.loadPersonalInfo(account.value)
  }

  // 弹窗复用已读结果，首次查看或手动刷新才查询，关闭不清除共享快照
  watch([open, key, () => account.value.capabilities.profile, () => account.value.capabilities.subscription], ([isOpen]) => {
    if (isOpen && !info.value)
      void load()
  }, { immediate: true })

  return {
    profile,
    subscription,
    loading,
    error,
    load,
  }
}
