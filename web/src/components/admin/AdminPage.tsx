import { FC } from 'react'
import { WithNavBar } from '@Components/WithNavbar'
import { WithRole } from '@Components/WithRole'
import { AdminTabProps, WithAdminTab } from '@Components/admin/WithAdminTab'
import { Role } from '@Api'

export const AdminPage: FC<AdminTabProps> = (props) => {
  return (
    <WithNavBar width="min(calc(100% - 32px), 1800px)">
      <WithRole requiredRole={Role.Admin} allowEventAdmin>
        <WithAdminTab {...props} />
      </WithRole>
    </WithNavBar>
  )
}
