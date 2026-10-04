import { Modal, ModalProps } from '@mantine/core'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'

export type AccessibleModalProps = Omit<ModalProps, 'stackId'>

/**
 * Mantine's default modal header is a top-level `<header>`, which screen readers
 * interpret as another page banner when the modal is rendered in a portal. Keep
 * Mantine's established modal visuals and dialog labelling while removing that
 * unintended landmark.
 */
export const AccessibleModal: FC<AccessibleModalProps> = ({
  title,
  withOverlay = true,
  overlayProps,
  withCloseButton = true,
  closeButtonProps,
  children,
  radius,
  ...rootProps
}) => {
  const { t } = useTranslation()
  const hasHeader = Boolean(title) || withCloseButton

  return (
    <Modal.Root radius={radius} {...rootProps}>
      {withOverlay && <Modal.Overlay {...overlayProps} />}
      <Modal.Content radius={radius}>
        {hasHeader && (
          <Modal.Header role="presentation">
            {title && <Modal.Title>{title}</Modal.Title>}
            {withCloseButton && (
              <Modal.CloseButton
                {...closeButtonProps}
                aria-label={closeButtonProps?.['aria-label'] ?? t('common.button.close', 'Close')}
              />
            )}
          </Modal.Header>
        )}
        <Modal.Body>{children}</Modal.Body>
      </Modal.Content>
    </Modal.Root>
  )
}
