import { Button, Group, Menu, Stack, Text, VisuallyHidden } from '@mantine/core'
import { mdiDeleteOutline, mdiDotsHorizontal, mdiEmailArrowRightOutline, mdiLockReset, mdiPencilOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import { useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { AccessibleModal } from '@Components/AccessibleModal'
import type { UserInfoModel } from '@Api'

type AccountAction = 'email' | 'reset' | 'delete'

/** A stable trigger owns confirmation and focus while the menu closes. */
export function UserActions(props: {
  user: UserInfoModel
  disabled: boolean
  canDelete: boolean
  onEdit: () => void
  onEmail: () => Promise<void>
  onReset: () => Promise<void>
  onDelete: () => Promise<void>
}) {
  const { t } = useTranslation()
  const trigger = useRef<HTMLButtonElement>(null)
  const busy = useRef(false)
  const [action, setAction] = useState<AccountAction | null>(null)
  const [running, setRunning] = useState(false)
  const name = props.user.userName ?? ''
  const actions = {
    email: {
      label: t('interface.users.password_link'),
      message: t('interface.users.email_confirm', { name }),
      run: props.onEmail,
    },
    reset: {
      label: t('interface.users.reset_password'),
      message: t('admin.content.users.reset.message', { name }),
      run: props.onReset,
    },
    delete: {
      label: t('interface.users.delete'),
      message: t('admin.content.users.delete', { name }),
      run: props.onDelete,
    },
  }
  const close = () => {
    if (!busy.current) setAction(null)
  }
  const confirm = async () => {
    if (!action || busy.current || props.disabled) return
    busy.current = true
    setRunning(true)
    try {
      await actions[action].run()
    } finally {
      busy.current = false
      setRunning(false)
      setAction(null)
    }
  }
  return (
    <>
      <Group
        gap="xs"
        wrap="nowrap"
        role="group"
        aria-label={t('interface.users.actions_for', { name })}
        data-user-actions={props.user.id}
      >
        <Button
          variant="default"
          size="sm"
          mih={44}
          disabled={props.disabled}
          leftSection={<Icon path={mdiPencilOutline} size={0.8} aria-hidden="true" />}
          onClick={props.onEdit}
        >
          {t('interface.actions.edit')}
        </Button>
        <Menu
          position="bottom-end"
          withinPortal
          width={250}
          withInitialFocusPlaceholder={false}
          returnFocus={false}
          onClose={() => trigger.current?.focus()}
        >
          <Menu.Target>
            <Button
              ref={trigger}
              variant="subtle"
              size="sm"
              mih={44}
              disabled={props.disabled || running}
              rightSection={<Icon path={mdiDotsHorizontal} size={0.85} aria-hidden="true" />}
            >
              {t('interface.actions.more')}
              <VisuallyHidden> {t('interface.users.actions_for', { name })}</VisuallyHidden>
            </Button>
          </Menu.Target>
          <Menu.Dropdown>
            <Menu.Item
              leftSection={<Icon path={mdiEmailArrowRightOutline} size={0.85} aria-hidden="true" />}
              disabled={!props.user.email}
              onClick={() => setAction('email')}
            >
              {actions.email.label}
            </Menu.Item>
            <Menu.Item
              leftSection={<Icon path={mdiLockReset} size={0.85} aria-hidden="true" />}
              onClick={() => setAction('reset')}
            >
              {actions.reset.label}
            </Menu.Item>
            <Menu.Divider />
            <Menu.Item
              color="red"
              c="light-dark(var(--mantine-color-red-9), var(--mantine-color-red-3))"
              leftSection={<Icon path={mdiDeleteOutline} size={0.85} aria-hidden="true" />}
              disabled={!props.canDelete}
              onClick={() => setAction('delete')}
            >
              {actions.delete.label}
            </Menu.Item>
          </Menu.Dropdown>
        </Menu>
      </Group>
      <AccessibleModal
        opened={action !== null}
        onClose={close}
        title={action ? actions[action].label : ''}
        withCloseButton={!running}
        closeOnClickOutside={!running}
        closeOnEscape={!running}
        onExitTransitionEnd={() => trigger.current?.focus()}
      >
        <Stack>
          <Text>{action ? actions[action].message : ''}</Text>
          <Group justify="flex-end">
            <Button variant="default" onClick={close} disabled={running}>
              {t('common.modal.cancel')}
            </Button>
            <Button
              autoContrast
              color={action === 'delete' ? 'red' : undefined}
              loading={running}
              disabled={props.disabled}
              onClick={() => void confirm()}
            >
              {t('common.modal.confirm')}
            </Button>
          </Group>
        </Stack>
      </AccessibleModal>
    </>
  )
}
