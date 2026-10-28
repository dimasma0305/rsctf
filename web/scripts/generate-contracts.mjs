import { mkdtemp, readFile, mkdir, writeFile, rm } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { generateApi } from 'swagger-typescript-api'

const web = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const temporary = await mkdtemp(path.join(os.tmpdir(), 'rsctf-contract-'))
try {
  await generateApi({
    input: path.join(web, '../openapi/v1.json'), output: temporary,
    fileName: 'BackendContract.ts', generateClient: false, enumStyle: 'union', sortTypes: true, sortRoutes: true,
    silent: true,
  })
  const generated = await readFile(path.join(temporary, 'BackendContract.ts'), 'utf8')
  const destination = path.join(web, 'src/contracts/BackendContract.ts')
  if (process.argv.includes('--check')) {
    const committed = await readFile(destination, 'utf8')
    if (committed !== generated) throw new Error('Generated API types drifted; run pnpm genapi')
    console.log('Generated API types match the committed OpenAPI contract.')
  } else {
    await mkdir(path.dirname(destination), { recursive: true })
    await writeFile(destination, generated)
    console.log('Updated src/contracts/BackendContract.ts; legacy client extensions preserved.')
  }
} finally { await rm(temporary, { recursive: true }) }
