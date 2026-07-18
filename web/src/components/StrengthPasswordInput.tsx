import { Box, Center, PasswordInput, PasswordInputProps, Popover, Progress, Text } from '@mantine/core'
import { useDisclosure } from '@mantine/hooks'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import React, { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { useIsMobile } from '@Utils/ThemeOverride'
import misc from '@Styles/Misc.module.css'

const PasswordRequirement: FC<{ meets: boolean; label: string }> = ({ meets, label }) => {
  return (
    <Text c={meets ? 'teal' : 'red'} mt={5} size="sm">
      <Center inline>
        {meets ? <Icon path={mdiCheck} size={0.7} /> : <Icon path={mdiClose} size={0.7} />}
        <Box ml={7}>{label}</Box>
      </Center>
    </Text>
  )
}

interface StrengthPasswordInputProps extends Omit<PasswordInputProps, 'value' | 'onChange'> {
  value: string
  onChange: React.ChangeEventHandler<HTMLInputElement>
}

export const StrengthPasswordInput: FC<StrengthPasswordInputProps> = (props) => {
  const { value, onChange, label, onFocusCapture, onBlurCapture, ...inputProps } = props
  const [opened, { close, open }] = useDisclosure(false)
  const pwd = value
  const isMobile = useIsMobile()

  const { t } = useTranslation()

  const requirements = [
    { re: /[0-9]/, label: t('account.password.include_number') },
    { re: /[a-z]/, label: t('account.password.include_lowercase') },
    { re: /[A-Z]/, label: t('account.password.include_uppercase') },
    { re: /[`$&+,:;=?@#|'<>.^*()%!-]/, label: t('account.password.include_symbol') },
  ]

  const getStrength = (password: string) => {
    let multiplier = password.length > 5 ? 0 : 1

    requirements.forEach((requirement) => {
      if (!requirement.re.test(password)) {
        multiplier += 1
      }
    })

    return Math.max(100 - (100 / (requirements.length + 1)) * multiplier, 0)
  }

  const checks = [
    <PasswordRequirement key={0} label={t('account.password.min_length')} meets={pwd.length >= 6} />,
    ...requirements.map((requirement, index) => (
      <PasswordRequirement key={index + 1} label={requirement.label} meets={requirement.re.test(pwd)} />
    )),
  ]

  const strength = getStrength(pwd)
  const color = strength === 100 ? 'teal' : strength > 50 ? 'yellow' : 'red'

  return (
    <Popover
      withArrow
      opened={opened}
      position={isMobile ? 'top' : 'right'}
      data-mobile={isMobile || undefined}
      classNames={{ dropdown: misc.dropdown }}
      transitionProps={{ transition: 'pop-bottom-left' }}
    >
      <Popover.Target>
        <PasswordInput
          required
          placeholder="P4ssW@rd"
          w="100%"
          {...inputProps}
          label={label ?? t('account.label.password')}
          value={value}
          onChange={onChange}
          onFocusCapture={(event) => {
            onFocusCapture?.(event)
            open()
          }}
          onBlurCapture={(event) => {
            onBlurCapture?.(event)
            close()
          }}
        />
      </Popover.Target>
      <Popover.Dropdown>
        <Progress color={color} value={strength} size={5} mb={10} />
        {checks}
      </Popover.Dropdown>
    </Popover>
  )
}
