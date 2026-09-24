import request from '../request'

export type ProviderRequestProfile = Record<string, unknown>
export type ProviderRequestProfiles = Record<string, ProviderRequestProfile>
export type ProviderRequestProfileUpdates = Record<string, ProviderRequestProfile | null>

export interface LegacyClientProfileSelection {
  mode?: undefined
  client: 'desktop' | 'cli'
  platform: 'macos' | 'linux' | 'windows'
  versionMode: 'latest' | 'fixed'
  originator: string | null
  osVersion: string | null
  arch: string | null
  terminal: string | null
  codexVersion: string | null
  desktopVersion: string | null
  desktopBuild: string | null
}

export interface ClientProfileCatalogEntry {
  client: 'desktop' | 'cli' | 'exec'
  environment: string
  release: string
  userAgent: string
}

export interface CatalogClientProfileSelection {
  mode: 'catalog'
  versionMode: 'latest' | 'fixed'
  entry: ClientProfileCatalogEntry
}

export interface CustomClientProfileSelection {
  mode: 'custom'
  userAgent: string
  originator?: string | null
  codexVersion?: string | null
}

export type ClientProfileSelection = LegacyClientProfileSelection | CatalogClientProfileSelection | CustomClientProfileSelection

export interface ClientProfileCatalogSource {
  source: 'desktop' | 'cli'
  checkedAt: string | null
  updatedAt: string | null
  error: string | null
}

export interface ClientProfileOptions {
  presets: ClientProfilePreset[]
  globalConfiguration: ClientProfileSelection
  catalog: {
    entries: ClientProfileCatalogEntry[]
    sources: ClientProfileCatalogSource[]
    releaseLimit: number
  }
}

export interface ClientProfilePreview {
  configuration: ClientProfileSelection
  source: 'global' | 'override'
  originator: string
  osType: string
  osVersion: string
  arch: string
  terminal: string
  codexVersion: string
  desktopVersion: string | null
  desktopBuild: string | null
  userAgent: string
  versionSource: 'official' | 'custom' | 'catalog'
  recognized: boolean
  verifiedAt: string | null
  checkedAt: string | null
  error: string | null
}

export interface ClientProfilePreset {
  configuration: LegacyClientProfileSelection
  automaticAvailable: boolean
  reason: string | null
  defaults: Pick<LegacyClientProfileSelection, 'originator' | 'osVersion' | 'arch' | 'terminal'>
}

export function getClientProfileOptions() {
  return request<ClientProfileOptions>({
    url: '/api/admin/settings/client-profiles/openai',
    method: 'GET',
    silent: true,
  })
}

export function refreshClientProfileCatalog() {
  return request<ClientProfileOptions>({
    url: '/api/admin/settings/client-profiles/openai/refresh',
    method: 'POST',
    silent: true,
    timeout: 135000,
  })
}

export function previewClientProfile(configuration: ClientProfileSelection | null) {
  return request<ClientProfilePreview>({
    url: '/api/admin/settings/client-profiles/openai/preview',
    method: 'POST',
    data: { configuration },
    silent: true,
  })
}

export interface XaiClientProfileSelection {
  versionMode: 'latest' | 'fixed'
  clientVersion: string | null
  clientIdentifier: string
  clientMode: string
  targetOs: string
  targetArch: string
}

export interface XaiClientProfilePreview extends Omit<XaiClientProfileSelection, 'versionMode'> {
  configuration: XaiClientProfileSelection
  source: 'global' | 'override'
  userAgent: string
  versionSource: 'official' | 'custom'
  verifiedAt: string | null
  checkedAt: string | null
  error: string | null
}

export function getXaiClientProfileOptions() {
  return request<{ defaults: XaiClientProfileSelection, globalConfiguration: XaiClientProfileSelection }>({
    url: '/api/admin/settings/client-profiles/xai',
    method: 'GET',
    silent: true,
  })
}

export function previewXaiClientProfile(configuration: XaiClientProfileSelection | null) {
  return request<XaiClientProfilePreview>({
    url: '/api/admin/settings/client-profiles/xai/preview',
    method: 'POST',
    data: { configuration },
    silent: true,
  })
}
