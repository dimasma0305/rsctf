import getOfflineAudioContext from '@creepjs/audio'
import getCanvas2d from '@creepjs/canvas'
import getCSSMedia from '@creepjs/cssmedia'
import getHTMLElementVersion from '@creepjs/document'
import getClientRects from '@creepjs/domrect'
import getConsoleErrors from '@creepjs/engine'
import { caniuse, getCapturedErrors } from '@creepjs/errors'
import getEngineFeatures from '@creepjs/features'
import getFonts from '@creepjs/fonts'
import getHeadlessFeatures from '@creepjs/headless'
import getIntl from '@creepjs/intl'
import { getLies } from '@creepjs/lies'
import getMaths from '@creepjs/math'
import getMedia from '@creepjs/media'
import getNavigator from '@creepjs/navigator'
import getResistance from '@creepjs/resistance'
import getScreen from '@creepjs/screen'
import getVoices from '@creepjs/speech'
import getSVG from '@creepjs/svg'
import getTimezone from '@creepjs/timezone'
import { getTrash } from '@creepjs/trash'
import { hashify } from '@creepjs/utils/crypto'
import {
  IS_BLINK,
  LowerEntropy,
  braveBrowser,
  getBraveMode,
  getBraveUnprotectedParameters,
} from '@creepjs/utils/helpers'
import getCanvasWebgl from '@creepjs/webgl'
import getWindowFeatures from '@creepjs/window'
import getBestWorkerScope from '@creepjs/worker'
import {
  collectRequiredFingerprintSignals,
  fingerprintProbeValue,
  FingerprintCollectionError,
  FingerprintProbeName,
  runFingerprintProbe,
} from '@Utils/FingerprintProbe'
import getCSS from '@creepjs/css'

interface FingerprintProof {
  version: number
  fingerprint: string
  nonce?: string
  signalOrder?: string[]
  signals?: Record<string, string>
  lieCount?: number
  trashCount?: number
  errorCount?: number
  headlessRating?: number
  stealthRating?: number
  likeHeadlessRating?: number
  resistance?: {
    privacy?: string
    mode?: string
    extension?: string
  }
}

export interface FingerprintChallenge {
  nonce: string
  requiredSignals: string[]
}

export interface FingerprintPayload {
  fingerprint: string
  proof: string
}

export interface FingerprintCollectionOptions {
  signal?: AbortSignal
  probeTimeoutMs?: number
}

const OPTIONAL_PROBE_TIMEOUT_MS = 4_000
const REQUIRED_PROBE_TIMEOUT_MS = 8_000

const requiredProbeNames = (requiredSignals: string[]): Set<FingerprintProbeName> => {
  const probes = new Set<FingerprintProbeName>(['fingerprintHash'])
  for (const signal of requiredSignals) {
    if (signal === 'lie_count') probes.add('lies')
    if (signal === 'headless_rating') probes.add('headless')
    if (signal === 'platform_consistent' || signal === 'ua_consistent') {
      probes.add('workerScope')
      probes.add('navigator')
    }
    if (signal === 'webgl_consistent') {
      probes.add('workerScope')
      probes.add('canvasWebgl')
    }
  }
  return probes
}

const toNonNegativeInt = (value: unknown): number | undefined => {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return undefined
  }
  return Math.max(0, Math.trunc(value))
}

const toPercentage = (value: unknown): number | undefined => {
  const parsed = toNonNegativeInt(value)
  return parsed === undefined ? undefined : Math.min(100, parsed)
}

const toOptionalString = (value: unknown): string | undefined => {
  if (typeof value !== 'string') {
    return undefined
  }
  const trimmed = value.trim()
  return trimmed.length === 0 ? undefined : trimmed
}

const buildFingerprintProof = (fp: any, fingerprint: string, challenge?: FingerprintChallenge): FingerprintProof => {
  const lieCount = toNonNegativeInt(fp?.lies?.totalLies)
  const trashCount =
    fp?.trash === undefined ? undefined : Array.isArray(fp?.trash?.trashBin) ? fp.trash.trashBin.length : undefined
  const errorCount =
    fp?.capturedErrors === undefined
      ? undefined
      : Array.isArray(fp?.capturedErrors?.data)
        ? fp.capturedErrors.data.length
        : undefined
  const headlessRating = toPercentage(fp?.headless?.headlessRating)
  const stealthRating = toPercentage(fp?.headless?.stealthRating)
  const likeHeadlessRating = toPercentage(fp?.headless?.likeHeadlessRating)
  const resistance = {
    privacy: toOptionalString(fp?.resistance?.privacy),
    mode: toOptionalString(fp?.resistance?.mode),
    extension: toOptionalString(fp?.resistance?.extension),
  }

  const requiredSignals = challenge?.requiredSignals ?? []
  const selectedSignals = collectRequiredFingerprintSignals(fp, requiredSignals)

  const proof: FingerprintProof = {
    version: 1,
    fingerprint,
    nonce: challenge?.nonce,
    signalOrder: requiredSignals,
    signals: selectedSignals,
    lieCount,
    trashCount,
    errorCount,
    headlessRating,
    stealthRating,
    likeHeadlessRating,
    resistance: resistance.privacy || resistance.mode || resistance.extension ? resistance : undefined,
  }
  return proof
}

export const getFingerprintPayload = async (
  challenge?: FingerprintChallenge,
  options: FingerprintCollectionOptions = {}
): Promise<FingerprintPayload> => {
  const requiredProbes = requiredProbeNames(challenge?.requiredSignals ?? [])
  const collect = (probe: FingerprintProbeName, task: () => any): Promise<any> =>
    runFingerprintProbe<any>(probe, task, {
      signal: options.signal,
      timeoutMs:
        options.probeTimeoutMs ?? (requiredProbes.has(probe) ? REQUIRED_PROBE_TIMEOUT_MS : OPTIONAL_PROBE_TIMEOUT_MS),
    }).then(fingerprintProbeValue)

  const brave = IS_BLINK
    ? await collect('brave', async () => {
        const isBrave = await braveBrowser()
        return { isBrave, mode: isBrave ? getBraveMode() : {} }
      })
    : undefined
  const isBrave = Boolean(brave?.isBrave)
  const braveMode: any = brave?.mode ?? {}
  const braveFingerprintingBlocking = isBrave && (braveMode.standard || braveMode.strict)

  const [
    workerScopeComputed,
    voicesComputed,
    offlineAudioContextComputed,
    canvasWebglComputed,
    canvas2dComputed,
    windowFeaturesComputed,
    htmlElementVersionComputed,
    cssComputed,
    cssMediaComputed,
    screenComputed,
    mathsComputed,
    consoleErrorsComputed,
    timezoneComputed,
    clientRectsComputed,
    fontsComputed,
    mediaComputed,
    svgComputed,
    resistanceComputed,
    intlComputed,
  ] = await Promise.all([
    collect('workerScope', getBestWorkerScope),
    collect('voices', getVoices),
    collect('offlineAudioContext', getOfflineAudioContext),
    collect('canvasWebgl', getCanvasWebgl),
    collect('canvas2d', getCanvas2d),
    collect('windowFeatures', getWindowFeatures),
    collect('htmlElementVersion', getHTMLElementVersion),
    collect('css', getCSS),
    collect('cssMedia', getCSSMedia),
    collect('screen', getScreen),
    collect('maths', getMaths),
    collect('consoleErrors', getConsoleErrors),
    collect('timezone', getTimezone),
    collect('clientRects', getClientRects),
    collect('fonts', getFonts),
    collect('media', getMedia),
    collect('svg', getSVG),
    collect('resistance', getResistance),
    collect('intl', getIntl),
  ])

  const navigatorComputed = await collect('navigator', () => getNavigator(workerScopeComputed))

  const [headlessComputed, featuresComputed] = await Promise.all([
    collect('headless', () =>
      getHeadlessFeatures({
        webgl: canvasWebglComputed,
        workerScope: workerScopeComputed,
      })
    ),
    collect('features', () =>
      getEngineFeatures({
        cssComputed,
        navigatorComputed,
        windowFeaturesComputed,
      })
    ),
  ])

  const [liesComputed, trashComputed, capturedErrorsComputed] = await Promise.all([
    collect('lies', getLies),
    collect('trash', getTrash),
    collect('capturedErrors', getCapturedErrors),
  ])

  const hardenEntropy = (workerScope: any, prop: any) => {
    return !workerScope
      ? prop
      : workerScope.localeEntropyIsTrusty && workerScope.localeIntlEntropyIsTrusty
        ? prop
        : undefined
  }

  const fp = {
    workerScope: workerScopeComputed,
    navigator: navigatorComputed,
    windowFeatures: windowFeaturesComputed,
    headless: headlessComputed,
    htmlElementVersion: htmlElementVersionComputed,
    cssMedia: cssMediaComputed,
    css: cssComputed,
    screen: screenComputed,
    voices: voicesComputed,
    media: mediaComputed,
    canvas2d: canvas2dComputed,
    canvasWebgl: canvasWebglComputed,
    maths: mathsComputed,
    consoleErrors: consoleErrorsComputed,
    timezone: timezoneComputed,
    clientRects: clientRectsComputed,
    offlineAudioContext: offlineAudioContextComputed,
    fonts: fontsComputed,
    lies: liesComputed,
    trash: trashComputed,
    capturedErrors: capturedErrorsComputed,
    svg: svgComputed,
    resistance: resistanceComputed,
    intl: intlComputed,
    features: featuresComputed,
  }

  // Construct the "creep" object which represents the stable fingerprint
  const privacyResistFingerprinting =
    // @ts-ignore
    fp.resistance && /^(tor browser|firefox)$/i.test(fp.resistance.privacy)

  // harden gpu
  const hardenGPU = (canvasWebgl: any) => {
    if (!canvasWebgl || !canvasWebgl.gpu) {
      return {}
    }
    const {
      gpu: { confidence, compressedGPU },
    } = canvasWebgl
    return confidence == 'low'
      ? {}
      : {
          UNMASKED_RENDERER_WEBGL: compressedGPU,
          UNMASKED_VENDOR_WEBGL: (canvasWebgl.parameters || {}).UNMASKED_VENDOR_WEBGL,
        }
  }

  const creep = {
    navigator:
      // @ts-ignore
      !fp.navigator || fp.navigator.lied
        ? undefined
        : {
            // @ts-ignore
            bluetoothAvailability: fp.navigator.bluetoothAvailability,
            // @ts-ignore
            device: fp.navigator.device,
            // @ts-ignore
            deviceMemory: fp.navigator.deviceMemory,
            // @ts-ignore
            hardwareConcurrency: fp.navigator.hardwareConcurrency,
            // @ts-ignore
            maxTouchPoints: fp.navigator.maxTouchPoints,
            // @ts-ignore
            oscpu: fp.navigator.oscpu,
            // @ts-ignore
            platform: fp.navigator.platform,
            // @ts-ignore
            system: fp.navigator.system,
            userAgentData: {
              // @ts-ignore
              ...(fp.navigator.userAgentData || {}),
              // loose
              brandsVersion: undefined,
              uaFullVersion: undefined,
            },
            // @ts-ignore
            vendor: fp.navigator.vendor,
          },
    screen:
      // @ts-ignore
      !fp.screen || fp.screen.lied || privacyResistFingerprinting || LowerEntropy.SCREEN
        ? undefined
        : hardenEntropy(fp.workerScope, {
            // @ts-ignore
            height: fp.screen.height,
            // @ts-ignore
            width: fp.screen.width,
            // @ts-ignore
            pixelDepth: fp.screen.pixelDepth,
            // @ts-ignore
            colorDepth: fp.screen.colorDepth,
            // @ts-ignore
            lied: fp.screen.lied,
          }),
    workerScope:
      !fp.workerScope || fp.workerScope.lied
        ? undefined
        : {
            deviceMemory: braveFingerprintingBlocking ? undefined : fp.workerScope.deviceMemory,
            hardwareConcurrency: braveFingerprintingBlocking ? undefined : fp.workerScope.hardwareConcurrency,
            // system locale in blink
            language: !LowerEntropy.TIME_ZONE ? fp.workerScope.language : undefined,
            platform: fp.workerScope.platform,
            system: fp.workerScope.system,
            device: fp.workerScope.device,
            timezoneLocation: !LowerEntropy.TIME_ZONE
              ? hardenEntropy(fp.workerScope, fp.workerScope.timezoneLocation)
              : undefined,
            webglRenderer:
              fp.workerScope && fp.workerScope.gpu && fp.workerScope.gpu.confidence != 'low'
                ? fp.workerScope.gpu.compressedGPU
                : undefined,
            webglVendor:
              fp.workerScope && fp.workerScope.gpu && fp.workerScope.gpu.confidence != 'low'
                ? fp.workerScope.webglVendor
                : undefined,
            userAgentData: fp.workerScope
              ? {
                  ...fp.workerScope.userAgentData,
                  // loose
                  brandsVersion: undefined,
                  uaFullVersion: undefined,
                }
              : undefined,
          },
    media: fp.media,
    canvas2d: ((canvas2d: any) => {
      if (!canvas2d) {
        return
      }
      const { lied, liedTextMetrics } = canvas2d
      let data
      if (!lied) {
        const { dataURI, paintURI, textURI, emojiURI } = canvas2d
        data = {
          lied,
          ...{ dataURI, paintURI, textURI, emojiURI },
        }
      }
      if (!liedTextMetrics) {
        const { textMetricsSystemSum, emojiSet } = canvas2d
        data = {
          ...(data || {}),
          ...{ textMetricsSystemSum, emojiSet },
        }
      }
      return data
    })(fp.canvas2d),
    canvasWebgl:
      !fp.canvasWebgl || fp.canvasWebgl.lied || LowerEntropy.WEBGL
        ? undefined
        : braveFingerprintingBlocking
          ? {
              parameters: {
                ...getBraveUnprotectedParameters(fp.canvasWebgl.parameters),
                ...hardenGPU(fp.canvasWebgl),
              },
            }
          : {
              ...((gl: any, canvas2d: any) => {
                if ((canvas2d && canvas2d.lied) || LowerEntropy.CANVAS) {
                  // distrust images
                  const { extensions, gpu, lied, parameterOrExtensionLie } = gl
                  return {
                    extensions,
                    gpu,
                    lied,
                    parameterOrExtensionLie,
                  }
                }
                return gl
              })(fp.canvasWebgl, fp.canvas2d),
              parameters: {
                ...fp.canvasWebgl.parameters,
                ...hardenGPU(fp.canvasWebgl),
              },
            },
    consoleErrors: fp.consoleErrors,
    cssMedia: !fp.cssMedia
      ? undefined
      : {
          // @ts-ignore
          reducedMotion: caniuse(() => fp.cssMedia.mediaCSS['prefers-reduced-motion']),
          colorScheme: braveFingerprintingBlocking
            ? undefined
            : // @ts-ignore
              caniuse(() => fp.cssMedia.mediaCSS['prefers-color-scheme']),
          // @ts-ignore
          monochrome: caniuse(() => fp.cssMedia.mediaCSS.monochrome),
          // @ts-ignore
          invertedColors: caniuse(() => fp.cssMedia.mediaCSS['inverted-colors']),
          // @ts-ignore
          forcedColors: caniuse(() => fp.cssMedia.mediaCSS['forced-colors']),
          // @ts-ignore
          anyHover: caniuse(() => fp.cssMedia.mediaCSS['any-hover']),
          // @ts-ignore
          hover: caniuse(() => fp.cssMedia.mediaCSS.hover),
          // @ts-ignore
          anyPointer: caniuse(() => fp.cssMedia.mediaCSS['any-pointer']),
          // @ts-ignore
          pointer: caniuse(() => fp.cssMedia.mediaCSS.pointer),
          // @ts-ignore
          colorGamut: caniuse(() => fp.cssMedia.mediaCSS['color-gamut']),
          screenQuery:
            privacyResistFingerprinting || LowerEntropy.SCREEN || LowerEntropy.IFRAME_SCREEN
              ? undefined
              : // @ts-ignore
                hardenEntropy(
                  fp.workerScope,
                  caniuse(() => fp.cssMedia.screenQuery)
                ),
        },
    // @ts-ignore
    css: !fp.css ? undefined : fp.css.system.fonts,
    timezone:
      !fp.timezone || fp.timezone.lied || LowerEntropy.TIME_ZONE
        ? undefined
        : {
            locationMeasured: hardenEntropy(fp.workerScope, fp.timezone.locationMeasured),
            lied: fp.timezone.lied,
          },
    offlineAudioContext: !fp.offlineAudioContext
      ? undefined
      : fp.offlineAudioContext.lied || LowerEntropy.AUDIO
        ? undefined
        : fp.offlineAudioContext,
    fonts: !fp.fonts || fp.fonts.lied || LowerEntropy.FONTS ? undefined : fp.fonts.fontFaceLoadFonts,
  }

  if (!Object.values(creep).some((value) => value !== undefined)) {
    throw new FingerprintCollectionError('fingerprint-unavailable')
  }
  const creepHash = await collect('fingerprintHash', () => hashify(creep))
  if (!creepHash) {
    throw new FingerprintCollectionError('fingerprint-unavailable')
  }
  const proof = buildFingerprintProof(fp, creepHash, challenge)

  return {
    fingerprint: creepHash,
    proof: JSON.stringify(proof),
  }
}

export const getFingerprint = async (options?: FingerprintCollectionOptions): Promise<string> =>
  (await getFingerprintPayload(undefined, options)).fingerprint
