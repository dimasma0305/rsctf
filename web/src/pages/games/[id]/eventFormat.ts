import type { TFunction } from 'i18next'

interface FormattableGameEvent {
  type: string
  values: string[]
}

const formatAnswer = (t: TFunction, result: string) => {
  switch (result) {
    case 'Accepted':
      return t('game.event.answer.accepted')
    case 'WrongAnswer':
      return t('game.event.answer.wrong')
    case 'CheatDetected':
      return t('game.event.answer.cheat')
    case 'FlagSubmitted':
      return t('game.event.answer.submitted')
    case 'NotFound':
      return t('game.event.answer.not_found')
    default:
      return ''
  }
}

export function formatGameEvent(t: TFunction, event: FormattableGameEvent) {
  switch (event.type) {
    case 'Normal':
      return event.values.at(-1) || ''
    case 'FlagSubmit':
      return t('game.event.flag_submit', {
        status: formatAnswer(t, event.values.at(0) ?? ''),
        flag: event.values.at(1),
        chal: event.values.at(2),
        id: event.values.at(3),
      })
    case 'CheatDetected':
      return t('game.event.cheat_detected', {
        chal: event.values.at(0),
        team: event.values.at(1),
        steam: event.values.at(2),
      })
    case 'ContainerStart':
      return t('game.event.container.start', {
        id: event.values.at(0),
        chal: event.values.at(1),
      })
    case 'Download': {
      // Canonical download values are [challengeId, challengeTitle, token].
      // The token is authorization-sensitive and must never be rendered. Fall
      // back to the stable challenge id when a row has no title so the monitor
      // event is still identifiable.
      const challengeId = event.values.at(0)?.trim()
      const challengeTitle = event.values.at(1)?.trim()
      const challenge = challengeTitle || (challengeId ? `#${challengeId}` : t('game.event.unknown_challenge'))
      return t('game.event.download', {
        chal: challenge,
        defaultValue: 'Downloaded: {{chal}}',
      })
    }
    case 'ContainerDestroy':
      return t('game.event.container.destroy', {
        id: event.values.at(0),
        chal: event.values.at(1),
      })
    case 'ChallengeOpened':
      return (
        t('game.event.challenge_opened', {
          chal: event.values.at(1),
        }) || `Opened challenge ${event.values.at(1)}`
      )
    default:
      return event.values.at(-1) || ''
  }
}
