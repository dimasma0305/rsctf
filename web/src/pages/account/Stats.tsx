import { FC } from 'react'
import { Navigate } from 'react-router'

// Stats were merged into the unified account page; keep the old URL working.
const Stats: FC = () => <Navigate to="/account/profile?tab=stats" replace />

export default Stats
