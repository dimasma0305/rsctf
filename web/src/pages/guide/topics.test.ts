import i18next from 'i18next'
import assert from 'node:assert/strict'
import test from 'node:test'
import { ContainerPortMappingType, type ClientConfig } from '@Api'
import en from '../../locales/en-US/guide.json'
import id from '../../locales/id-ID/guide.json'
import { filterGuideTopics, guideTopics, selectedGuideTopic } from './topics'

test('guide topics preserve bookmarks and use effective account, team and connection policies', async () => {
  const i18n = i18next.createInstance()
  await i18n.init({ lng: 'en', resources: { en: { translation: { guide: en } }, id: { translation: { guide: id } } } })
  const config = {
    allowRegister: false,
    allowTeamCreation: false,
    portMapping: ContainerPortMappingType.PlatformProxy,
  } as ClientConfig
  const topics = guideTopics(config, false, i18n.t)
  assert.equal(topics.length, 8)
  assert.equal(new Set(topics.map((topic) => topic.id)).size, topics.length)
  for (const id of ['account-access', 'find-event', 'join-event', 'play-challenge', 'submit-flag']) {
    assert.equal(selectedGuideTopic(topics, `#${id}`).id, id)
  }
  assert.equal(selectedGuideTopic(topics, '#<script>bad</script>').id, 'account-access')
  assert.match(topics[0].note!, /Registration is closed/)
  assert.equal(topics[0].action.to, '/account/login')
  assert.match(topics[2].note!, /creation is disabled/)
  assert.match(topics[4].note!, /Platform Proxy/)
  const available = guideTopics(
    {
      ...config,
      allowRegister: true,
      allowPasswordRegistration: false,
      enableDiscordAuth: true,
      allowTeamCreation: true,
      portMapping: ContainerPortMappingType.Default,
    },
    true,
    i18n.t
  )
  assert.match(available[0].note!, /Discord/)
  assert.equal(available[0].action.to, '/account/profile')
  assert.match(available[2].note!, /Create a team/)
  assert.match(available[4].note!, /direct host and port/)
  assert.equal(available[3].action.to, '/challenges')
  assert.ok(filterGuideTopics(topics, '  WSRX   netcat ').some((topic) => topic.id === 'connections'))
  assert.deepEqual(filterGuideTopics(topics, 'unmatched-phrase-123'), [])
  assert.equal(filterGuideTopics(topics, ' ').length, topics.length)
  await i18n.changeLanguage('id')
  const indonesian = guideTopics(config, false, i18n.t)
  assert.equal(indonesian[0].title, 'Akun & tim Anda')
  assert.ok(filterGuideTopics(indonesian, 'koneksi').some((topic) => topic.id === 'connections'))
  for (const topic of topics) {
    assert.equal(en.topics[topic.id].steps.length, id.topics[topic.id].steps.length)
    assert.equal(en.topics[topic.id].details.length, id.topics[topic.id].details.length)
  }
})
