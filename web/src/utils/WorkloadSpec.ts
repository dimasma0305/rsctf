import type { WorkloadSpec, WorkerImageIdentity, WorkerServiceSpec } from '@Api'

const MAX_SERVICES = 32
const MAX_PORTS = 32
const MAX_REPLICAS = 64
const MAX_WORKLOAD_REPLICAS = 512
const MAX_ENVIRONMENT_ENTRIES = 128
const MAX_ENVIRONMENT_KEY_BYTES = 128
const MAX_ENVIRONMENT_VALUE_BYTES = 16 * 1024
const MAX_ARCHITECTURE_BYTES = 64
const MAX_WINDOWS_BUILD_BYTES = 128
const MAX_REGISTRY_REPOSITORY_BYTES = 255
const MAX_FLAG_PATH_BYTES = 1024
const MAX_WORKLOAD_SPEC_BYTES = 192 * 1024
const MAX_U32 = 4_294_967_295

const DNS_LABEL = /^(?!-)[a-z0-9-]{1,63}(?<!-)$/
const ENVIRONMENT_KEY = /^[A-Za-z_][A-Za-z0-9_]*$/
const SHA256_DIGEST = /^sha256:[0-9a-fA-F]{64}$/
const UUID = /^[0-9a-fA-F]{8}-(?:[0-9a-fA-F]{4}-){3}[0-9a-fA-F]{12}$/
const ARCHITECTURE = /^[A-Za-z0-9._-]+$/
const REPOSITORY_COMPONENT = /^[a-z0-9]+(?:(?:\.|_{1,2}|-+)[a-z0-9]+)*$/

export type WorkloadSpecParseResult =
  | { ok: true; value: WorkloadSpec }
  | {
      ok: false
      error: string
    }

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value)

const isPositiveSafeInteger = (value: unknown): value is number =>
  typeof value === 'number' && Number.isSafeInteger(value) && value > 0

const byteLength = (value: string) => new TextEncoder().encode(value).length

const invalid = (error: string): WorkloadSpecParseResult => ({ ok: false, error })

function validRegistryPort(value: string): boolean {
  if (!/^[0-9]+$/.test(value)) return false
  const port = Number(value)
  return Number.isSafeInteger(port) && port > 0 && port <= 65_535
}

function validRegistryDomain(value: string): boolean {
  if (value.startsWith('[')) {
    const closing = value.indexOf(']')
    if (closing < 0) return false
    const address = value.slice(1, closing)
    const suffix = value.slice(closing + 1)
    if (suffix && (!suffix.startsWith(':') || !validRegistryPort(suffix.slice(1)))) return false
    try {
      return new URL(`http://[${address}]`).hostname.startsWith('[')
    } catch {
      return false
    }
  }

  const colon = value.lastIndexOf(':')
  const hasPort = colon >= 0
  const host = hasPort ? value.slice(0, colon) : value
  const port = hasPort ? value.slice(colon + 1) : null
  if (!host || host.includes(':') || (port !== null && !validRegistryPort(port))) return false
  return host.split('.').every(
    (label) =>
      label.length >= 1 &&
      label.length <= 63 &&
      /^[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?$/.test(label)
  )
}

function validRegistryRepository(value: string): boolean {
  if (
    !value ||
    byteLength(value) > MAX_REGISTRY_REPOSITORY_BYTES ||
    !/^[\x00-\x7f]+$/.test(value) ||
    value.includes('@')
  ) {
    return false
  }
  const components = value.split('/')
  if (components.some((component) => !component)) return false
  const first = components[0]
  const hasDomain =
    components.length > 1 &&
    (first === 'localhost' || first.includes('.') || first.includes(':') || first.startsWith('['))
  if (hasDomain && !validRegistryDomain(first)) return false
  return components.slice(hasDomain ? 1 : 0).every((component) => REPOSITORY_COMPONENT.test(component))
}

function validateObjectKeys(value: Record<string, unknown>, path: string, allowed: readonly string[]): string | null {
  const unknown = Object.keys(value).find((key) => !allowed.includes(key))
  return unknown ? `${path}.${unknown} is not supported` : null
}

function validateImage(image: unknown, path: string): string | null {
  if (!isRecord(image)) return `${path} must be an object`
  if (image.type === 'registryDigest') {
    const keyError = validateObjectKeys(image, path, ['type', 'repository', 'digest'])
    if (keyError) return keyError
    if (typeof image.repository !== 'string' || !validRegistryRepository(image.repository)) {
      return `${path}.repository must be a canonical registry repository without a tag or digest`
    }
    if (typeof image.digest !== 'string' || !SHA256_DIGEST.test(image.digest)) {
      return `${path}.digest must be a sha256 digest`
    }
    return null
  }
  if (image.type === 'workerLocal') {
    const keyError = validateObjectKeys(image, path, ['type', 'workerId', 'imageId'])
    if (keyError) return keyError
    if (typeof image.workerId !== 'string' || !UUID.test(image.workerId)) {
      return `${path}.workerId must be a UUID`
    }
    if (typeof image.imageId !== 'string' || !SHA256_DIGEST.test(image.imageId)) {
      return `${path}.imageId must be a sha256 image ID`
    }
    return null
  }
  return `${path}.type must be "registryDigest" or "workerLocal"`
}

function validateEnvironment(environment: unknown, path: string): string | null {
  if (environment === undefined) return null
  if (!isRecord(environment)) return `${path} must be an object of string values`
  const entries = Object.entries(environment)
  if (entries.length > MAX_ENVIRONMENT_ENTRIES) return `${path} cannot contain more than 128 entries`
  for (const [key, value] of entries) {
    if (byteLength(key) > MAX_ENVIRONMENT_KEY_BYTES || !ENVIRONMENT_KEY.test(key)) {
      return `${path}.${key} has an invalid environment-variable name`
    }
    if (typeof value !== 'string' || byteLength(value) > MAX_ENVIRONMENT_VALUE_BYTES || value.includes('\0')) {
      return `${path}.${key} must be a bounded string without NUL bytes`
    }
  }
  return null
}

function validateService(service: unknown, index: number): string | null {
  const path = `services[${index}]`
  if (!isRecord(service)) return `${path} must be an object`
  const keyError = validateObjectKeys(service, path, [
    'name',
    'image',
    'resources',
    'replicas',
    'stateless',
    'environment',
    'ports',
  ])
  if (keyError) return keyError
  if (typeof service.name !== 'string' || !DNS_LABEL.test(service.name)) {
    return `${path}.name must be a lowercase DNS label`
  }

  const imageError = validateImage(service.image, `${path}.image`)
  if (imageError) return imageError

  if (!isRecord(service.resources)) return `${path}.resources must be an object`
  const resourceKeyError = validateObjectKeys(service.resources, `${path}.resources`, ['cpuMillis', 'memoryBytes'])
  if (resourceKeyError) return resourceKeyError
  if (!isPositiveSafeInteger(service.resources.cpuMillis) || service.resources.cpuMillis > MAX_U32) {
    return `${path}.resources.cpuMillis must be a positive 32-bit integer`
  }
  if (!isPositiveSafeInteger(service.resources.memoryBytes)) {
    return `${path}.resources.memoryBytes must be a positive safe integer`
  }
  if (
    typeof service.replicas !== 'number' ||
    !Number.isInteger(service.replicas) ||
    service.replicas < 1 ||
    service.replicas > MAX_REPLICAS
  ) {
    return `${path}.replicas must be an integer from 1 to 64`
  }
  if (typeof service.stateless !== 'boolean') return `${path}.stateless must be a boolean`
  if (service.replicas > 1 && !service.stateless) {
    return `${path}.stateless must be true when replicas is greater than one`
  }

  const environmentError = validateEnvironment(service.environment, `${path}.environment`)
  if (environmentError) return environmentError

  if (!Array.isArray(service.ports) || service.ports.length < 1 || service.ports.length > MAX_PORTS) {
    return `${path}.ports must contain between 1 and 32 ports`
  }
  const names = new Set<string>()
  const numbers = new Set<number>()
  for (const [portIndex, port] of service.ports.entries()) {
    const portPath = `${path}.ports[${portIndex}]`
    if (!isRecord(port)) return `${portPath} must be an object`
    const portKeyError = validateObjectKeys(port, portPath, ['name', 'containerPort', 'protocol'])
    if (portKeyError) return portKeyError
    if (typeof port.name !== 'string' || !DNS_LABEL.test(port.name) || names.has(port.name)) {
      return `${portPath}.name must be a unique lowercase DNS label`
    }
    names.add(port.name)
    if (
      typeof port.containerPort !== 'number' ||
      !Number.isInteger(port.containerPort) ||
      port.containerPort < 1 ||
      port.containerPort > 65_535 ||
      numbers.has(port.containerPort)
    ) {
      return `${portPath}.containerPort must be a unique integer from 1 to 65535`
    }
    numbers.add(port.containerPort)
    if (port.protocol !== 'tcp') return `${portPath}.protocol must be "tcp"`
  }
  return null
}

function validateWorkload(value: unknown): WorkloadSpecParseResult {
  if (!isRecord(value)) return invalid('the root value must be an object')
  const rootKeyError = validateObjectKeys(value, 'workload', [
    'gameKind',
    'platform',
    'services',
    'primaryEndpoint',
    'flagTarget',
  ])
  if (rootKeyError) return invalid(rootKeyError)
  if (value.gameKind !== 'jeopardy') return invalid('gameKind must be "jeopardy"')

  if (!isRecord(value.platform)) return invalid('platform must be an object')
  const platformKeyError = validateObjectKeys(value.platform, 'platform', [
    'operatingSystem',
    'architecture',
    'windowsBuild',
  ])
  if (platformKeyError) return invalid(platformKeyError)
  if (value.platform.operatingSystem !== 'linux' && value.platform.operatingSystem !== 'windows') {
    return invalid('platform.operatingSystem must be "linux" or "windows"')
  }
  if (
    typeof value.platform.architecture !== 'string' ||
    !ARCHITECTURE.test(value.platform.architecture) ||
    byteLength(value.platform.architecture) > MAX_ARCHITECTURE_BYTES ||
    !/^[\x00-\x7f]+$/.test(value.platform.architecture)
  ) {
    return invalid('platform.architecture must be a non-empty OCI architecture without whitespace')
  }
  if (value.platform.windowsBuild != null) {
    if (
      typeof value.platform.windowsBuild !== 'string' ||
      !value.platform.windowsBuild.trim() ||
      byteLength(value.platform.windowsBuild) > MAX_WINDOWS_BUILD_BYTES
    ) {
      return invalid('platform.windowsBuild must be a non-empty bounded string')
    }
    if (value.platform.operatingSystem === 'linux') {
      return invalid('a Linux platform cannot declare windowsBuild')
    }
  }

  if (!Array.isArray(value.services) || value.services.length < 1 || value.services.length > MAX_SERVICES) {
    return invalid('services must contain between 1 and 32 services')
  }
  const serviceNames = new Set<string>()
  let totalReplicas = 0
  for (const [index, service] of value.services.entries()) {
    const error = validateService(service, index)
    if (error) return invalid(error)
    const name = (service as WorkerServiceSpec).name
    if (serviceNames.has(name)) return invalid(`service name "${name}" is duplicated`)
    serviceNames.add(name)
    totalReplicas += (service as WorkerServiceSpec).replicas
    if (totalReplicas > MAX_WORKLOAD_REPLICAS) {
      return invalid('services cannot request more than 512 replicas in total')
    }
  }

  if (!isRecord(value.primaryEndpoint)) return invalid('primaryEndpoint must be an object')
  const primaryEndpoint = value.primaryEndpoint
  const endpointKeyError = validateObjectKeys(primaryEndpoint, 'primaryEndpoint', ['service', 'port'])
  if (endpointKeyError) return invalid(endpointKeyError)
  if (typeof primaryEndpoint.service !== 'string' || typeof primaryEndpoint.port !== 'string') {
    return invalid('primaryEndpoint.service and primaryEndpoint.port must be strings')
  }
  const endpointExists = (value.services as WorkerServiceSpec[]).some(
    (service) =>
      service.name === primaryEndpoint.service && service.ports.some((port) => port.name === primaryEndpoint.port)
  )
  if (!endpointExists) return invalid('primaryEndpoint must select an existing named service port')

  if (!isRecord(value.flagTarget)) return invalid('flagTarget is required and must be an object')
  const flagTargetKeyError = validateObjectKeys(value.flagTarget, 'flagTarget', ['service', 'path'])
  if (flagTargetKeyError) return invalid(flagTargetKeyError)
  if (typeof value.flagTarget.service !== 'string' || !serviceNames.has(value.flagTarget.service)) {
    return invalid('flagTarget.service must select an existing service')
  }
  if (
    typeof value.flagTarget.path !== 'string' ||
    !value.flagTarget.path.trim() ||
    byteLength(value.flagTarget.path) > MAX_FLAG_PATH_BYTES ||
    value.flagTarget.path.includes('\0')
  ) {
    return invalid('flagTarget.path must be a non-empty bounded guest path')
  }

  if (byteLength(JSON.stringify(value)) > MAX_WORKLOAD_SPEC_BYTES) {
    return invalid('the workload specification cannot exceed 192 KiB')
  }

  return { ok: true, value: value as unknown as WorkloadSpec }
}

export function parseJeopardyWorkloadSpec(input: string): WorkloadSpecParseResult {
  let value: unknown
  try {
    value = JSON.parse(input)
  } catch (error) {
    return invalid(error instanceof Error ? error.message : 'invalid JSON')
  }
  return validateWorkload(value)
}

export function createDefaultJeopardyWorkloadSpec(): WorkloadSpec {
  const image: WorkerImageIdentity = {
    type: 'registryDigest',
    repository: 'registry.example/ctf/challenge',
    digest: `sha256:${'0'.repeat(64)}`,
  }
  return {
    gameKind: 'jeopardy',
    platform: {
      operatingSystem: 'linux',
      architecture: 'amd64',
    },
    services: [
      {
        name: 'challenge',
        image,
        resources: {
          cpuMillis: 500,
          memoryBytes: 134_217_728,
        },
        replicas: 1,
        stateless: true,
        environment: {},
        ports: [{ name: 'service', containerPort: 8080, protocol: 'tcp' }],
      },
    ],
    primaryEndpoint: { service: 'challenge', port: 'service' },
    flagTarget: { service: 'challenge', path: '/flag' },
  }
}

export const formatWorkloadSpec = (spec: WorkloadSpec) => JSON.stringify(spec, null, 2)
