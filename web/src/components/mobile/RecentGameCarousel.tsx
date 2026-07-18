import { Carousel, CarouselProps } from '@mantine/carousel'
import { Box, Button, Group, VisuallyHidden } from '@mantine/core'
import { useReducedMotion } from '@mantine/hooks'
import { mdiPause, mdiPlay } from '@mdi/js'
import { Icon } from '@mdi/react'
import Autoplay from 'embla-carousel-autoplay'
import { FC, FocusEvent, MouseEvent, PointerEvent, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { RecentGameSlide } from '@Components/mobile/RecentGameSlide'
import { BasicGameInfoModel } from '@Api'
import classes from '@Styles/RecentGameCarousel.module.css'
import '@mantine/carousel/styles.css'

interface RecentGameCarouselProps extends CarouselProps {
  games: BasicGameInfoModel[]
}

export const RecentGameCarousel: FC<RecentGameCarouselProps> = ({
  games,
  onMouseEnter,
  onMouseLeave,
  onFocusCapture,
  onBlurCapture,
  onPointerUp,
  ...props
}) => {
  const { t } = useTranslation()
  const reducedMotion = useReducedMotion()
  const autoplay = useRef(
    Autoplay({ delay: 5000, stopOnInteraction: false, stopOnFocusIn: false, playOnInit: !reducedMotion })
  )
  const pointerInside = useRef(false)
  const focusInside = useRef(false)
  const [autoplayPaused, setAutoplayPaused] = useState(reducedMotion)

  useEffect(() => {
    if (reducedMotion) {
      setAutoplayPaused(true)
      autoplay.current.stop()
    }
  }, [reducedMotion])

  const resumeWhenIdle = (paused = autoplayPaused) => {
    if (!paused && !pointerInside.current && !focusInside.current) {
      autoplay.current.play()
    }
  }

  const handleMouseEnter = (event: MouseEvent<HTMLDivElement>) => {
    pointerInside.current = true
    autoplay.current.stop()
    onMouseEnter?.(event)
  }

  const handleMouseLeave = (event: MouseEvent<HTMLDivElement>) => {
    pointerInside.current = false
    resumeWhenIdle()
    onMouseLeave?.(event)
  }

  const handleFocus = (event: FocusEvent<HTMLDivElement>) => {
    focusInside.current = true
    autoplay.current.stop()
    onFocusCapture?.(event)
  }

  const handleBlur = (event: FocusEvent<HTMLDivElement>) => {
    if (!event.currentTarget.contains(event.relatedTarget)) {
      focusInside.current = false
      resumeWhenIdle()
    }
    onBlurCapture?.(event)
  }

  const handlePointerUp = (event: PointerEvent<HTMLDivElement>) => {
    if (autoplayPaused) autoplay.current.stop()
    onPointerUp?.(event)
  }

  const toggleAutoplay = () => {
    const nextPaused = !autoplayPaused
    setAutoplayPaused(nextPaused)

    if (nextPaused) {
      autoplay.current.stop()
    } else {
      resumeWhenIdle(false)
    }
  }

  return (
    <Box w="100%" mx="auto" className={classes.root}>
      {games.length > 1 && (
        <Group justify="flex-end" mb="xs">
          <Button
            type="button"
            size="compact-sm"
            variant="subtle"
            className={classes.rotationButton}
            leftSection={<Icon path={autoplayPaused ? mdiPlay : mdiPause} size={0.7} aria-hidden />}
            onClick={toggleAutoplay}
          >
            {autoplayPaused
              ? t('game.content.recent_games.resume_slides', 'Resume slides')
              : t('game.content.recent_games.pause_slides', 'Pause slides')}
          </Button>
          <VisuallyHidden aria-live="polite">
            {autoplayPaused
              ? t('game.content.recent_games.rotation_paused', 'Automatic slide rotation paused')
              : t('game.content.recent_games.rotation_enabled', 'Automatic slide rotation enabled')}
          </VisuallyHidden>
        </Group>
      )}
      <Carousel
        className={classes.carousel}
        type="container"
        withIndicators
        slideGap="md"
        withControls={false}
        plugins={[autoplay.current]}
        emblaOptions={{
          loop: games.length > 1,
        }}
        aria-label={t('game.content.recent_games.label', 'Recent games')}
        onMouseEnter={handleMouseEnter}
        onMouseLeave={handleMouseLeave}
        onFocusCapture={handleFocus}
        onBlurCapture={handleBlur}
        onPointerUp={handlePointerUp}
        {...props}
      >
        {games.map((game, index) => (
          <Carousel.Slide
            key={game.id}
            role="group"
            aria-label={t('game.content.recent_games.slide_label', 'Game {{index}} of {{count}}: {{title}}', {
              index: index + 1,
              count: games.length,
              title: game.title ?? t('game.content.recent_games.untitled', 'Untitled game'),
            })}
          >
            <RecentGameSlide game={game} />
          </Carousel.Slide>
        ))}
      </Carousel>
    </Box>
  )
}
