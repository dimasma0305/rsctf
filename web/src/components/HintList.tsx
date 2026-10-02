import { ActionIcon, Badge, Button, Group, Input, InputWrapperProps, ScrollArea, Stack, TextInput } from '@mantine/core'
import { mdiClose, mdiLightbulbOffOutline, mdiLightbulbOnOutline, mdiPlus } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'

interface HintListProps extends InputWrapperProps {
  hints: string[]
  onChangeHint: (value: string[]) => void
  disabled?: boolean
  height?: number
  releasedHintCount?: number
  onReleaseHint?: (index: number) => void
  onUnreleaseHint?: (index: number) => void
  releaseDisabled?: boolean
  releasingHint?: boolean
  unreleasingHint?: boolean
}

export const HintList: FC<HintListProps> = (props) => {
  const {
    hints,
    onChangeHint,
    disabled,
    height,
    releasedHintCount = 0,
    onReleaseHint,
    onUnreleaseHint,
    releaseDisabled,
    releasingHint,
    unreleasingHint,
    ...rest
  } = props

  const { t } = useTranslation()

  const hintdict = hints.map((hint, key) => ({ hint, key }))

  const handleChange = (key: number, value: string) => {
    const newHints = [...hintdict]
    newHints[key].hint = value
    onChangeHint(newHints.map((h) => h.hint))
  }

  const handleAdd = () => {
    const newHints = [...hintdict, { hint: '', key: hintdict.length }]
    onChangeHint(newHints.map((h) => h.hint))
  }

  const handleDelete = (key: number) => {
    const newHints = hintdict.filter((h) => h.key !== key)
    onChangeHint(newHints.map((h) => h.hint))
  }

  return (
    <Input.Wrapper {...rest}>
      <ScrollArea offsetScrollbars scrollbarSize={4} h={height}>
        <Stack gap="xs">
          {hintdict.map((kv) => {
            const released = kv.key < releasedHintCount
            const next = kv.key === releasedHintCount
            const lastReleased = released && kv.key === releasedHintCount - 1
            return (
              <Stack gap={4} key={kv.key} mr={4}>
                <TextInput
                  value={kv.hint}
                  aria-label={t('admin.content.games.challenges.hint_number', 'Hint {{number}}', {
                    number: kv.key + 1,
                  })}
                  disabled={disabled}
                  onChange={(e) => handleChange(kv.key, e.target.value)}
                  rightSection={
                    <ActionIcon
                      aria-label={t('admin.button.challenges.hint.delete', 'Delete hint {{number}}', {
                        number: kv.key + 1,
                      })}
                      disabled={disabled}
                      onClick={() => handleDelete(kv.key)}
                    >
                      <Icon path={mdiClose} size={1} />
                    </ActionIcon>
                  }
                />
                <Group justify="space-between" gap="xs" wrap="wrap">
                  <Badge color={released ? 'teal' : 'gray'} variant="light">
                    {released
                      ? t('admin.content.games.challenges.hint_released', 'Released')
                      : t('admin.content.games.challenges.hint_draft', 'Draft')}
                  </Badge>
                  {lastReleased && onUnreleaseHint && (
                    <Button
                      size="compact-xs"
                      variant="light"
                      color="gray"
                      leftSection={<Icon path={mdiLightbulbOffOutline} size={0.8} />}
                      disabled={disabled || releaseDisabled}
                      loading={unreleasingHint}
                      onClick={() => onUnreleaseHint(kv.key)}
                    >
                      {t('admin.button.challenges.hint.unrelease', 'Unrelease hint {{number}}', {
                        number: kv.key + 1,
                      })}
                    </Button>
                  )}
                  {!released && onReleaseHint && (
                    <Button
                      size="compact-xs"
                      variant="light"
                      leftSection={<Icon path={mdiLightbulbOnOutline} size={0.8} />}
                      disabled={disabled || releaseDisabled || !next || !kv.hint.trim()}
                      loading={releasingHint && next}
                      onClick={() => onReleaseHint(kv.key)}
                    >
                      {t('admin.button.challenges.hint.release', 'Release hint {{number}}', { number: kv.key + 1 })}
                    </Button>
                  )}
                </Group>
              </Stack>
            )
          })}
          <Button mr={4} leftSection={<Icon path={mdiPlus} size={1} />} disabled={disabled} onClick={handleAdd}>
            {t('admin.button.challenges.hint.add')}
          </Button>
        </Stack>
      </ScrollArea>
    </Input.Wrapper>
  )
}
