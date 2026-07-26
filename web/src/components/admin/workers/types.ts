export type WorkerState = 'Enabled' | 'Draining' | 'Disabled'
export type WorkerFilter = 'all' | 'online' | 'offline' | 'enabled' | 'draining' | 'disabled'

export interface WorkerCapacity {
  cpuMillis: number
  memoryBytes: number
  slots: number
}

export interface Worker {
  id: string
  name: string
  administrativeState: WorkerState
  platformOs?: string | null
  architecture?: string | null
  runtimeKind?: string | null
  runtimeVersion?: string | null
  capacity: WorkerCapacity
  online: boolean
  heartbeatAt?: number | null
}

export interface Enrollment {
  workerId: string
  token: string
  expiresAt: number
}

export interface CreatedWorker {
  worker: Worker
  enrollment: Enrollment
}

export interface WorkerInstallCommands {
  linux: string
  windows: string
  linuxUninstall: string
  windowsUninstall: string
}
