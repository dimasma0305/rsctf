import { Accordion, Alert, Button, Group, Progress, Stack, Switch, Text, TextInput, Title } from '@mantine/core'
import { mdiArrowLeft, mdiArrowRight, mdiInformationOutline, mdiMagnify } from '@mdi/js'
import { Icon } from '@mdi/react'
import { useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useLocation } from 'react-router'
import { PageHeader } from '@Components/PageHeader'
import { WithNavBar } from '@Components/WithNavbar'
import { usePlayerGuide } from '@Components/guide/PlayerGuide'
import { GUIDE_TOUR_STEPS, GUIDE_VERSION } from '@Utils/GuideState'
import { useConfig } from '@Hooks/useConfig'
import { usePageTitle } from '@Hooks/usePageTitle'
import { useUser } from '@Hooks/useUser'
import classes from '@Styles/PlayerGuidePage.module.css'
import { filterGuideTopics, guideTopics, selectedGuideTopic } from './topics'

const Guide = () => {
  const { t } = useTranslation()
  const { config } = useConfig()
  const { user } = useUser()
  const guide = usePlayerGuide()
  const { hash, search: routeSearch } = useLocation()
  const [search, setSearch] = useState('')
  const heading = useRef<HTMLHeadingElement>(null)
  const topics = guideTopics(config, Boolean(user), t)
  const topic = selectedGuideTopic(topics, hash)
  const visibleTopics = filterGuideTopics(topics, search)
  const index = topics.findIndex((item) => item.id === topic.id)
  const tourIndex = guide.preferences.activeTourStep ? GUIDE_TOUR_STEPS.indexOf(guide.preferences.activeTourStep) : -1
  const completed = tourIndex < 0 && guide.preferences.completedVersion >= GUIDE_VERSION
  const tourLabel =
    tourIndex >= 0
      ? t('guide.hub.resume', 'Resume walkthrough')
      : completed
        ? t('guide.hub.replay', 'Replay walkthrough')
        : t('guide.hub.start', 'Start walkthrough')
  usePageTitle(t('common.tab.guide', 'Guide'))

  const focusTopic = () =>
    requestAnimationFrame(() => {
      heading.current?.focus({ preventScroll: true })
      heading.current?.scrollIntoView({
        block: window.matchMedia('(max-width: 48em)').matches ? 'start' : 'nearest',
        behavior: 'instant',
      })
    })

  return (
    <WithNavBar withFooter withHeader stickyHeader>
      <PageHeader
        title={t('common.content.about.player_guide', 'Player guide')}
        description={t('guide.hub.description', 'Pick a topic, or follow the walkthrough on the real controls.')}
      />
      <div className={classes.page} data-guide-hub>
        <section className={classes.tourCard} aria-labelledby="guide-tour-title">
          <div className={classes.tourCopy}>
            <Title order={2} size="sm" id="guide-tour-title">
              {t('guide.hub.tour_title', 'Interactive walkthrough')}
            </Title>
            <Text size="sm" c="dimmed">
              {t('guide.hub.tour_body', 'A short walkthrough of the platform. You stay in control of every action.')}
            </Text>
            {(tourIndex >= 0 || completed) && (
              <div className={classes.progress}>
                <Text size="xs" role="status">
                  {completed
                    ? t('guide.hub.complete', 'Walkthrough completed')
                    : t('guide.hub.progress', { current: tourIndex + 1, total: GUIDE_TOUR_STEPS.length })}
                </Text>
                <Progress
                  value={completed ? 100 : (tourIndex / GUIDE_TOUR_STEPS.length) * 100}
                  aria-label={t('guide.hub.tour_title', 'Learn by doing')}
                  size={4}
                />
              </div>
            )}
          </div>
          <Button
            data-guide-start
            disabled={!guide.ready}
            onClick={guide.startGuide}
            rightSection={<Icon path={mdiArrowRight} size={0.8} aria-hidden="true" />}
          >
            {tourLabel}
          </Button>
        </section>
        <div className={classes.workspace}>
          <aside className={classes.sidebar}>
            <TextInput
              label={t('guide.hub.search', 'Find help')}
              placeholder={t('guide.hub.placeholder', 'Try VPN, team, flag…')}
              leftSection={<Icon path={mdiMagnify} size={0.8} aria-hidden="true" />}
              value={search}
              onChange={(event) => setSearch(event.currentTarget.value)}
              maxLength={160}
              data-guide-search
            />
            <nav aria-label={t('guide.hub.topics', 'Guide topics')} className={classes.contents}>
              {visibleTopics.map((item) => (
                <Link
                  key={item.id}
                  to={{ hash: `#${item.id}`, search: routeSearch }}
                  aria-current={item.id === topic.id ? 'page' : undefined}
                  className={classes.topicLink}
                  onClick={focusTopic}
                  data-guide-topic={item.id}
                >
                  <Icon path={item.icon} size={0.9} aria-hidden="true" />
                  <span>{item.title}</span>
                  <Icon path={mdiArrowRight} size={0.7} aria-hidden="true" />
                </Link>
              ))}
            </nav>
            {search && (
              <Text size="xs" c="dimmed" role="status">
                {visibleTopics.length
                  ? t('guide.hub.results', { count: visibleTopics.length })
                  : t('guide.hub.empty', 'No matching topics. Try “VPN”, “team” or “score”.')}
              </Text>
            )}
            {search && (
              <Button variant="subtle" size="compact-sm" onClick={() => setSearch('')}>
                {t('guide.hub.clear', 'Clear search')}
              </Button>
            )}
            <Accordion variant="default" className={classes.preferences}>
              <Accordion.Item value="preferences">
                <Accordion.Control>{t('guide.hub.settings', 'Guide preferences')}</Accordion.Control>
                <Accordion.Panel>
                  <Stack gap="sm">
                    <Switch
                      checked={guide.preferences.interactiveEnabled}
                      disabled={!guide.ready}
                      label={t('guide.hub.tips', 'Show contextual tips')}
                      onChange={(event) => guide.setInteractiveEnabled(event.currentTarget.checked)}
                    />
                    <Text size="xs" c="dimmed">
                      {t('guide.hub.tips_help', 'One-time help for attachments, instances and event VPN.')}
                    </Text>
                    <Button variant="default" disabled={!guide.ready} onClick={guide.resetGuide}>
                      {t('guide.hub.reset', 'Restart walkthrough')}
                    </Button>
                  </Stack>
                </Accordion.Panel>
              </Accordion.Item>
            </Accordion>
          </aside>
          <article
            id={topic.id}
            className={classes.article}
            aria-labelledby="guide-topic-title"
            data-guide-article={topic.id}
          >
            <header className={classes.topicHeader}>
              <Title order={2} ref={heading} tabIndex={-1} id="guide-topic-title">
                {topic.title}
              </Title>
              <Text c="dimmed">{topic.summary}</Text>
            </header>
            <div key={topic.id} className={classes.topicBody} data-motion="surface">
              {topic.note && (
                <Alert icon={<Icon path={mdiInformationOutline} size={0.9} aria-hidden="true" />}>{topic.note}</Alert>
              )}
              <section aria-labelledby="guide-steps-title">
                <Title order={3} size="sm" id="guide-steps-title" mb="md">
                  {t('guide.hub.steps', 'What to do')}
                </Title>
                <ol className={classes.steps}>
                  {topic.steps.map((step, stepIndex) => (
                    <li key={stepIndex}>
                      <span aria-hidden="true">{stepIndex + 1}</span>
                      <p>{step}</p>
                    </li>
                  ))}
                </ol>
              </section>
              <section aria-labelledby="guide-details-title">
                <Title order={3} size="sm" id="guide-details-title" mb="xs">
                  {t('guide.hub.details', 'Good to know')}
                </Title>
                <Accordion variant="default" multiple>
                  {topic.details.map((detail, detailIndex) => (
                    <Accordion.Item key={detailIndex} value={String(detailIndex)}>
                      <Accordion.Control>{detail.title}</Accordion.Control>
                      <Accordion.Panel>
                        <Text size="sm">{detail.body}</Text>
                      </Accordion.Panel>
                    </Accordion.Item>
                  ))}
                </Accordion>
              </section>
              <Group gap="sm" className={classes.actions}>
                <Button
                  component={Link}
                  to={topic.action.to}
                  rightSection={<Icon path={mdiArrowRight} size={0.8} aria-hidden="true" />}
                >
                  {topic.action.label}
                </Button>
                {topic.tourStep && (
                  <Button variant="default" disabled={!guide.ready} onClick={() => guide.startGuideAt(topic.tourStep!)}>
                    {t('guide.hub.practice', 'Show me in the app')}
                  </Button>
                )}
              </Group>
              <Text size="xs" c="dimmed" className={classes.policy}>
                {t(
                  'guide.hub.policy',
                  'Event rules decide eligibility, connections and scoring. The guide does not act for you.'
                )}
              </Text>
            </div>
            <footer className={classes.topicFooter}>
              {index > 0 ? (
                <Link to={{ hash: `#${topics[index - 1].id}`, search: routeSearch }} onClick={focusTopic}>
                  <Icon path={mdiArrowLeft} size={0.8} aria-hidden="true" />
                  <span>
                    <small>{t('guide.hub.previous', 'Previous topic')}</small>
                    {topics[index - 1].title}
                  </span>
                </Link>
              ) : (
                <span />
              )}
              {index < topics.length - 1 && (
                <Link to={{ hash: `#${topics[index + 1].id}`, search: routeSearch }} onClick={focusTopic}>
                  <span>
                    <small>{t('guide.hub.next', 'Next topic')}</small>
                    {topics[index + 1].title}
                  </span>
                  <Icon path={mdiArrowRight} size={0.8} aria-hidden="true" />
                </Link>
              )}
            </footer>
          </article>
        </div>
      </div>
    </WithNavBar>
  )
}

export default Guide
