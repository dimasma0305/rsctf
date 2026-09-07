import {
  mdiAccountGroupOutline,
  mdiCompassOutline,
  mdiFlagOutline,
  mdiHelpCircleOutline,
  mdiLanConnect,
  mdiLogin,
  mdiPodium,
  mdiViewGridOutline,
} from '@mdi/js'
import type { TFunction } from 'i18next'
import type { GuideTourStep } from '@Utils/GuideState'
import { ContainerPortMappingType, type ClientConfig } from '@Api'
import english from '../../locales/en-US/guide.json'

export type GuideTopicId = keyof typeof english.topics
export interface GuideTopic {
  id: GuideTopicId
  icon: string
  title: string
  summary: string
  steps: string[]
  details: { title: string; body: string }[]
  note?: string
  tourStep?: GuideTourStep
  action: { label: string; to: string }
}

export const guideTopics = (config: ClientConfig, signedIn: boolean, t: TFunction): GuideTopic[] => {
  const text = (key: keyof typeof english.hub) => t(`guide.hub.${key}`, english.hub[key])
  const login = { label: text('login'), to: '/account/login' }
  const games = { label: text('games'), to: '/games' }
  const challengeAction = signedIn ? { label: text('challenges'), to: '/challenges' } : login
  const providers = [config.enableGoogleAuth && 'Google', config.enableDiscordAuth && 'Discord']
    .filter(Boolean)
    .join(' / ')
  const accountNote =
    config.allowRegister === false
      ? text('registration_closed')
      : config.allowPasswordRegistration === false
        ? t('guide.hub.registration_oauth', {
            defaultValue: english.hub.registration_oauth,
            providers: providers || text('provider_fallback'),
          })
        : text('registration_password')
  const metadata: Record<GuideTopicId, Pick<GuideTopic, 'icon' | 'tourStep' | 'action' | 'note'>> = {
    'account-access': {
      icon: mdiAccountGroupOutline,
      tourStep: 'account',
      action: signedIn ? { label: text('account'), to: '/account/profile' } : login,
      note: [accountNote, config.emailConfirmationRequired && text('confirm_email')].filter(Boolean).join(' '),
    },
    'find-event': { icon: mdiCompassOutline, tourStep: 'events', action: games },
    'join-event': {
      icon: mdiLogin,
      tourStep: 'team',
      action: signedIn ? { label: text('teams'), to: '/teams' } : login,
      note: config.allowTeamCreation === false ? text('assigned_team') : text('create_team'),
    },
    'play-challenge': { icon: mdiViewGridOutline, tourStep: 'challenges', action: challengeAction },
    connections: {
      icon: mdiLanConnect,
      tourStep: 'connection',
      action: challengeAction,
      note:
        config.portMapping === ContainerPortMappingType.PlatformProxy ? text('proxy_default') : text('direct_default'),
    },
    'submit-flag': { icon: mdiFlagOutline, tourStep: 'submit', action: challengeAction },
    scoring: { icon: mdiPodium, action: games },
    troubleshooting: { icon: mdiHelpCircleOutline, action: games },
  }
  return (Object.keys(english.topics) as GuideTopicId[]).map((id) => {
    const source = english.topics[id]
    const translate = (key: string, fallback: string) => t(`guide.topics.${id}.${key}`, fallback)
    return {
      id,
      ...metadata[id],
      title: translate('title', source.title),
      summary: translate('summary', source.summary),
      steps: source.steps.map((step, index) => translate(`steps.${index}`, step)),
      details: source.details.map((detail, index) => ({
        title: translate(`details.${index}.title`, detail.title),
        body: translate(`details.${index}.body`, detail.body),
      })),
    }
  })
}

export const filterGuideTopics = (topics: GuideTopic[], query: string) => {
  const terms = query.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean)
  return topics.filter((topic) => {
    const text = [
      topic.title,
      topic.summary,
      ...topic.steps,
      topic.note,
      ...topic.details.flatMap((detail) => [detail.title, detail.body]),
    ]
      .join(' ')
      .toLocaleLowerCase()
    return terms.every((term) => text.includes(term))
  })
}

export const selectedGuideTopic = (topics: GuideTopic[], hash: string) =>
  topics.find((topic) => `#${topic.id}` === hash) ?? topics[0]
