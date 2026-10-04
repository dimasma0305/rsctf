import { Avatar, Badge, Button, Group, Text, Title } from '@mantine/core'
import {
  mdiArrowRight,
  mdiBookOpenPageVariantOutline,
  mdiCodeBraces,
  mdiFileDocumentOutline,
  mdiFlagOutline,
  mdiGithub,
  mdiOpenInNew,
  mdiScaleBalance,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import contributorsData from 'virtual:contributors'
import { Copyright } from '@Components/Copyright'
import { PageHeader } from '@Components/PageHeader'
import { WithNavBar } from '@Components/WithNavbar'
import { MainIcon } from '@Components/icon/MainIcon'
import { useIsMobile } from '@Utils/ThemeOverride'
import { RSCTF_DOCUMENTATION, ValidatedRepoMeta } from '@Hooks/useConfig'
import { usePageTitle } from '@Hooks/usePageTitle'
import classes from '@Styles/About.module.css'
import logoClasses from '@Styles/LogoHeader.module.css'

const About: FC = () => {
  const { repo, valid, rawTag: tag, sha, buildTime } = ValidatedRepoMeta()
  const { t } = useTranslation()
  const isMobile = useIsMobile()
  usePageTitle(t('common.title.about'))

  const modes = [
    {
      name: 'Jeopardy',
      description: t('common.content.about.jeopardy', 'Find the flag. Solve the challenge.'),
    },
    {
      name: 'Attack & Defense',
      description: t('common.content.about.ad', 'Defend your service. Challenge the others.'),
    },
    {
      name: 'King of the Hill',
      description: t('common.content.about.koth', 'Take the lead. Hold your ground.'),
    },
  ]
  const resources = [
    {
      href: '/guide',
      internal: true,
      icon: mdiBookOpenPageVariantOutline,
      title: t('common.content.about.player_guide', 'Player guide'),
      description: t('common.content.about.guide_description', 'From joining a team to submitting your first flag.'),
    },
    {
      href: RSCTF_DOCUMENTATION,
      icon: mdiFileDocumentOutline,
      title: t('common.content.about.documentation'),
      description: t(
        'common.content.about.docs_description',
        'Run the platform, build challenges, and understand the rules.'
      ),
    },
    {
      href: repo,
      icon: mdiGithub,
      title: t('common.content.about.repository'),
      description: t(
        'common.content.about.repo_description',
        'Explore the code, report an issue, or contribute a change.'
      ),
    },
  ]

  return (
    <WithNavBar>
      <div className={classes.page} data-about-page>
        <section className={classes.hero} aria-label={t('common.title.about')}>
          <div className={classes.heroCopy}>
            <PageHeader
              title={
                <>
                  <MainIcon size="2.25rem" aria-hidden="true" />
                  RS<span className={logoClasses.brand}>::</span>CTF
                </>
              }
            />
            <Text className={classes.intro}>
              {t('common.content.about.intro', 'An open-source capture-the-flag platform built with Rust and React.')}
            </Text>
            <Group gap="sm" className={classes.heroActions}>
              <Button
                component={Link}
                to="/games"
                leftSection={<Icon path={mdiFlagOutline} size={0.8} aria-hidden="true" />}
              >
                {t('common.content.about.explore_events', 'Explore events')}
              </Button>
              <Button
                component={Link}
                to="/guide"
                variant="default"
                rightSection={<Icon path={mdiArrowRight} size={0.8} aria-hidden="true" />}
              >
                {t('common.content.about.player_guide', 'Player guide')}
              </Button>
            </Group>
          </div>
        </section>

        <section className={classes.modes} aria-label={t('common.content.about.formats', 'Competition formats')}>
          {modes.map((mode) => (
            <div className={classes.mode} key={mode.name}>
              <div>
                <Title order={2}>{mode.name}</Title>
                <Text>{mode.description}</Text>
              </div>
            </div>
          ))}
        </section>

        <section aria-labelledby="about-resources-title" className={classes.section}>
          <div className={classes.sectionHeading}>
            <Title order={2} id="about-resources-title">
              {t('common.content.about.resources')}
            </Title>
          </div>
          <div className={classes.resources}>
            {resources.map((resource) => {
              const content = (
                <>
                  <Icon path={resource.icon} size={1} aria-hidden="true" />
                  <div className={classes.resourceCopy}>
                    <Title order={3}>{resource.title}</Title>
                    <Text>{resource.description}</Text>
                  </div>
                  <span className={classes.resourceArrow}>
                    <Icon path={resource.internal ? mdiArrowRight : mdiOpenInNew} size={0.85} aria-hidden="true" />
                  </span>
                </>
              )
              return resource.internal ? (
                <Link key={resource.href} to={resource.href} className={classes.resourceCard}>
                  {content}
                </Link>
              ) : (
                <a
                  key={resource.href}
                  href={resource.href}
                  target="_blank"
                  rel="noreferrer"
                  className={classes.resourceCard}
                >
                  {content}
                </a>
              )
            })}
          </div>
        </section>

        <div className={classes.projectGrid}>
          <section className={classes.projectPanel} aria-labelledby="about-contributors-title">
            <Title order={2} id="about-contributors-title">
              {t('common.content.about.contributors')}
            </Title>
            <Text className={classes.panelDescription}>
              {t('common.content.about.contributors_description', 'The people helping build and improve RSCTF.')}
            </Text>
            <ul className={classes.contributors}>
              {contributorsData.map((contributor) => (
                <li key={contributor.login}>
                  <a href={contributor.html_url} target="_blank" rel="noreferrer" className={classes.contributorLink}>
                    <Avatar
                      src={contributor.avatar_url}
                      alt=""
                      aria-hidden="true"
                      size={36}
                      imageProps={{ loading: 'lazy' }}
                    >
                      {contributor.login.slice(0, 1)}
                    </Avatar>
                    <span>@{contributor.login}</span>
                    <Icon path={mdiOpenInNew} size={0.75} aria-hidden="true" />
                  </a>
                </li>
              ))}
            </ul>
          </section>
          <section className={classes.projectPanel} aria-labelledby="about-build-title">
            <Title order={2} id="about-build-title">
              {t('common.content.about.version')}
            </Title>
            <div className={classes.buildRow}>
              <Icon path={mdiCodeBraces} size={1.1} aria-hidden="true" />
              <Badge variant="light" className={classes.versionBadge}>
                {valid ? tag : t('common.content.about.local_build', 'Source build')}
              </Badge>
            </div>
            {valid ? (
              <dl className={classes.buildDetails}>
                <div>
                  <dt>{t('common.content.about.revision', 'Revision')}</dt>
                  <dd>
                    <code className={classes.revision} title={sha}>
                      {sha.slice(0, 8)}
                    </code>
                  </dd>
                </div>
                <div>
                  <dt>{t('common.content.about.built', 'Built')}</dt>
                  <dd>
                    <time dateTime={buildTime.toISOString()}>{buildTime.format('YYYY-MM-DD HH:mm [UTC]Z')}</time>
                  </dd>
                </div>
              </dl>
            ) : (
              <Text className={classes.panelDescription}>
                {t('common.content.about.source_build', 'Source build · revision metadata unavailable')}
              </Text>
            )}
          </section>
        </div>

        <section className={classes.legal} aria-labelledby="about-legal-title">
          <div className={classes.legalCopy}>
            <Icon path={mdiScaleBalance} size={1.15} aria-hidden="true" />
            <div>
              <Title order={2} id="about-legal-title">
                {t('common.content.about.legal_title', 'Licensing & acknowledgements')}
              </Title>
              <Text>
                {t(
                  'common.content.about.legal_description',
                  'Licensing varies by component. See the guide and third-party notices for details.'
                )}
              </Text>
            </div>
          </div>
          <div className={classes.legalLinks}>
            <a href="/legal/LICENSING.md" target="_blank" rel="noreferrer">
              {t('common.content.about.licensing_guide', 'Licensing guide')}
              <Icon path={mdiOpenInNew} size={0.7} aria-hidden="true" />
            </a>
            <a href="/legal/third-party/CreepJS-LICENSE.txt" target="_blank" rel="noreferrer">
              {t('common.content.about.creepjs_license', 'CreepJS license')}
              <Icon path={mdiOpenInNew} size={0.7} aria-hidden="true" />
            </a>
          </div>
        </section>
        <footer className={classes.copyright}>
          <Copyright isMobile={isMobile} />
        </footer>
      </div>
    </WithNavBar>
  )
}

export default About
