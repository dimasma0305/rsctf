import { Center, Divider, Grid, Group, Image, Input, Stack, Text, Textarea, Title } from '@mantine/core'
import { Dropzone } from '@mantine/dropzone'
import { showNotification } from '@mantine/notifications'
import { mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { Dispatch, FC, SetStateAction } from 'react'
import { useTranslation } from 'react-i18next'
import { IMAGE_MIME_TYPES } from '@Utils/Shared'
import type { GameInfoModel } from '@Api'
import misc from '@Styles/Misc.module.css'

interface GameContentSectionProps {
  disabled: boolean
  game: GameInfoModel | undefined
  onUpdatePoster: (file: File | undefined) => Promise<void>
  setGame: Dispatch<SetStateAction<GameInfoModel | undefined>>
}

export const GameContentSection: FC<GameContentSectionProps> = ({ disabled, game, onUpdatePoster, setGame }) => {
  const { t } = useTranslation()
  return (
    <Stack gap="sm">
      <Title order={2}>{t('admin.content.games.info.section.content', 'Description & media')}</Title>
      <Divider />
      <Grid grow>
        <Grid.Col span={8}>
          <Textarea
            label={
              <Group gap="sm">
                <Text size="sm">{t('admin.content.games.info.content')}</Text>
                <Text size="xs" c="dimmed">
                  {t('admin.content.markdown_support')}
                </Text>
              </Group>
            }
            value={game?.content ?? ''}
            w="100%"
            autosize
            disabled={disabled}
            minRows={10}
            maxRows={10}
            onChange={(event) => game && setGame({ ...game, content: event.target.value })}
          />
        </Grid.Col>
        <Grid.Col span={4}>
          <Input.Wrapper label={t('admin.content.games.info.poster')}>
            <Dropzone
              onDrop={(files) => void onUpdatePoster(files[0])}
              onReject={() => {
                showNotification({
                  color: 'red',
                  title: t('common.error.file_invalid.title'),
                  message: t('common.error.file_invalid.message'),
                  icon: <Icon path={mdiClose} size={1} />,
                })
              }}
              maxSize={3 * 1024 * 1024}
              accept={IMAGE_MIME_TYPES}
              disabled={disabled}
              data-poster={game?.poster || undefined}
              classNames={{ root: misc.gamePoster }}
            >
              <Center className={misc.noPointerEvents}>
                {game?.poster ? (
                  <Image height="231px" fit="contain" src={game.poster} alt={t('admin.content.games.info.poster')} />
                ) : (
                  <Center h="231px">
                    <Stack gap={0}>
                      <Text size="xl" inline>
                        {t('common.content.drop_zone.content', {
                          type: t('common.content.drop_zone.type.poster'),
                        })}
                      </Text>
                      <Text size="sm" c="dimmed" inline mt={7}>
                        {t('common.content.drop_zone.limit')}
                      </Text>
                    </Stack>
                  </Center>
                )}
              </Center>
            </Dropzone>
          </Input.Wrapper>
        </Grid.Col>
      </Grid>
    </Stack>
  )
}
