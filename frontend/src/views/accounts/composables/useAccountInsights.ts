import type { InjectionKey, Ref } from 'vue'
import type { Account, AccountPersonalInfoResponse, AccountQuotaForecastResponse } from '@/api'
import { inject, onScopeDispose, provide, shallowReactive, watch } from 'vue'
import { getAccountPersonalInfo, getAccountQuotaForecast } from '@/api'
import { errorMessage } from '@/utils/operation'

export type AccountInsightIdentity = Pick<Account, 'id'> & Partial<Pick<Account, 'provider' | 'resourceRef' | 'accountId' | 'userId' | 'authenticationKind'>>

interface PersonalInfoState {
  identity: string
  info: AccountPersonalInfoResponse | null
  loading: boolean
  error: string
}

interface ForecastState {
  report: AccountQuotaForecastResponse | null
  loading: boolean
  error: boolean
}

const insightsKey: InjectionKey<ReturnType<typeof createAccountInsights>> = Symbol('account-insights')

export function accountInsightKey(account: AccountInsightIdentity) {
  return JSON.stringify([account.id, account.provider, account.resourceRef, account.accountId, account.userId, account.authenticationKind])
}

function createAccountInsights(accounts: Ref<Account[]>) {
  // 每个账号仅保留当前身份的读取结果，筛选、翻页和普通更新时间不使快照失效
  const personalStates = new Map<string, PersonalInfoState>()
  const personalRequests = new Map<string, { controller: AbortController, promise: Promise<boolean> }>()
  const forecasts = new Map<Account, ForecastState>()
  const requests = new Map<Account, { controller: AbortController, promise: Promise<boolean> }>()

  function personalInfo(account: AccountInsightIdentity) {
    const identity = accountInsightKey(account)
    let state = personalStates.get(account.id)
    if (!state || state.identity !== identity) {
      personalRequests.get(account.id)?.controller.abort()
      personalRequests.delete(account.id)
      state = shallowReactive<PersonalInfoState>({ identity, info: null, loading: false, error: '' })
      personalStates.set(account.id, state)
    }
    return state
  }

  function loadPersonalInfo(account: AccountInsightIdentity) {
    const state = personalInfo(account)
    const pending = personalRequests.get(account.id)
    if (pending)
      return pending.promise
    const controller = new AbortController()
    state.loading = true
    state.error = ''
    const promise = getAccountPersonalInfo({ accountId: account.id }, { signal: controller.signal, silent: true })
      .then((result) => {
        if (controller.signal.aborted)
          return false
        // 订阅 null 表示未知；个人统计明确失败时保留上次成功的统计
        state.info = {
          ...result,
          profile: result.profile ?? (result.profileError ? state.info?.profile ?? null : null),
        }
        return true
      })
      .catch((cause: unknown) => {
        if (!controller.signal.aborted)
          state.error = errorMessage(cause)
        return false
      })
      .finally(() => {
        state.loading = false
        if (personalRequests.get(account.id)?.controller === controller)
          personalRequests.delete(account.id)
      })
    personalRequests.set(account.id, { controller, promise })
    return promise
  }

  function forecast(account: Account) {
    let state = forecasts.get(account)
    if (!state) {
      state = shallowReactive<ForecastState>({ report: null, loading: false, error: false })
      forecasts.set(account, state)
    }
    return state
  }

  function loadForecast(account: Account) {
    const pending = requests.get(account)
    if (pending)
      return pending.promise
    const state = forecast(account)
    const controller = new AbortController()
    state.loading = true
    state.error = false
    // 预测只读取本地历史；列表展示不会触发上游额度刷新
    const promise = getAccountQuotaForecast({ accountId: account.id }, { signal: controller.signal, silent: true })
      .then((result) => {
        if (controller.signal.aborted)
          return false
        state.report = result
        return true
      })
      .catch(() => {
        if (!controller.signal.aborted)
          state.error = true
        return false
      })
      .finally(() => {
        state.loading = false
        requests.delete(account)
      })
    requests.set(account, { controller, promise })
    return promise
  }

  // 列表回读更新本地预测；个人信息仅在稳定身份变化时失效
  watch(accounts, (rows) => {
    const visible = new Set(rows)
    for (const account of rows) {
      if (personalStates.has(account.id))
        personalInfo(account)
    }
    for (const account of forecasts.keys()) {
      if (visible.has(account))
        continue
      requests.get(account)?.controller.abort()
      forecasts.delete(account)
    }
  })

  onScopeDispose(() => {
    for (const request of personalRequests.values())
      request.controller.abort()
    for (const request of requests.values())
      request.controller.abort()
  })

  return { personalInfo, loadPersonalInfo, forecast, loadForecast }
}

export function provideAccountInsights(accounts: Ref<Account[]>) {
  provide(insightsKey, createAccountInsights(accounts))
}

export function useAccountInsights() {
  const insights = inject(insightsKey)
  if (!insights)
    throw new Error('账号信息只能在账号页面使用')
  return insights
}
