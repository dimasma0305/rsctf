import { readFile, readdir } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')

export function controllerDependencies(source) {
  // This is a dependency ratchet, not a Rust parser. Ignore standalone prose;
  // retain concrete references in code and embedded source assertions.
  return source.split('\n').filter((line) => !line.trimStart().startsWith('//'))
    .flatMap((line) => [...line.matchAll(/\b(?:crate::)?controllers::[a-zA-Z_][a-zA-Z_0-9:]*/g)]
      .map((match) => match[0].replace(/^crate::/, '').replace(/:+$/, '')))
}

async function sources(directory) {
  const result = []
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const file = path.join(directory, entry.name)
    if (entry.isDirectory()) result.push(...await sources(file))
    else if (entry.isFile() && entry.name.endsWith('.rs')) result.push(file)
  }
  return result
}

export async function checkArchitecture(repository = root) {
  const allowed = JSON.parse(await readFile(path.join(repository, 'scripts/architecture-legacy.json'), 'utf8'))
  const errors = []
  const counts = {}
  for (const file of await sources(path.join(repository, 'src'))) {
    const relative = path.relative(repository, file).split(path.sep).join('/')
    const source = await readFile(file, 'utf8')
    const lines = source.trimEnd().split('\n').length
    if (lines > 1000) errors.push(`${relative}: ${lines} lines exceeds the 1000-line module limit`)
    const naming = relative.startsWith('src/bin/') ? /^[a-z][a-z0-9_-]*\.rs$/ : /^[a-z][a-z0-9_]*\.rs$/
    if (!naming.test(path.basename(file))) errors.push(`${relative}: Rust module names must use snake_case (binary names may contain hyphens)`)
    if (!relative.startsWith('src/services/')) continue
    for (const dependency of controllerDependencies(source)) {
      const key = `${relative} -> ${dependency}`
      counts[key] = (counts[key] ?? 0) + 1
    }
  }
  for (const [key, count] of Object.entries(counts)) {
    if (count > (allowed[key] ?? 0)) errors.push(`${key}: extract the shared operation into an application service`)
  }
  return { errors, counts }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const { errors } = await checkArchitecture()
  if (errors.length) { console.error(errors.join('\n')); process.exitCode = 1 }
  else console.log('Architecture checks passed: module size, naming, and service dependency direction.')
}
