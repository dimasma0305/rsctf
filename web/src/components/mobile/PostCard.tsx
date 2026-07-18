import { FC } from 'react'
import { PostCard, PostCardProps } from '@Components/PostCard'

/**
 * Post cards now adapt at the component boundary, so mobile and desktop share
 * one information hierarchy, admin controls, metadata treatment, and keyboard
 * behavior. Keep this wrapper to preserve existing imports and call sites.
 */
export const MobilePostCard: FC<PostCardProps> = (props) => <PostCard {...props} />
