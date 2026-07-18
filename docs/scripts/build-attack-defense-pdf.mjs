#!/usr/bin/env node

import { spawn, spawnSync } from 'node:child_process'
import { constants as fsConstants } from 'node:fs'
import { access, mkdir, mkdtemp, readFile, rename, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { dirname, isAbsolute, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const DOCS_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const HOST = '127.0.0.1'
const START_TIMEOUT_MS = 30_000
const PAGE_TIMEOUT_MS = 45_000
const PNPM = process.platform === 'win32' ? 'pnpm.cmd' : 'pnpm'
const PUBLIC_DOCS_BASE_URL = (
  process.env.DOCS_PUBLIC_ORIGIN?.trim() || ''
).replace(/\/+$/, '')
const REPOSITORY = process.env.GITHUB_REPOSITORY?.trim() || 'dimasma0305/rsctf'
const REPOSITORY_URL = `https://github.com/${REPOSITORY}`
const LICENSING_URL = `${REPOSITORY_URL}/blob/main/LICENSING.md`
const CREEPJS_LICENSE_URL = `${REPOSITORY_URL}/blob/main/web/src/lib/creepjs/LICENSE`
const PDF_PROFILES = {
  ad: {
    outputName: 'attack-defense-handbook.pdf',
    pagePath: '/players/attack-defense',
    expectedHeading: 'Attack & Defense',
    headerLabel: 'EpochBalanced A&amp;D Scoring · Implementation Report',
    metadata: {
      title: 'How RSCTF Scores Attack & Defense: The EpochBalanced Model',
      author: 'Dimas Maulana',
      subject: 'Technical practice paper and implementation report for rsctf Attack & Defense scoring',
      keywords:
        'attack-defense CTF; cybersecurity competition; epoch scoring; service-level agreement; human-AI teaming; rsctf',
    },
  },
  koth: {
    outputName: 'king-of-the-hill-scoring-handbook.pdf',
    pagePath: '/players/koth-scoring-handbook',
    expectedHeading: 'King of the Hill',
    headerLabel: 'Crown-Cycle KotH Scoring · Fixed Formula',
    generateDocumentOutline: false,
    metadata: {
      title: 'How RSCTF Scores King of the Hill: The Crown-Cycle Model',
      author: 'Dimas Maulana',
      subject: 'Technical practice paper for the fixed RSCTF crown-cycle King of the Hill scoring formula',
      keywords:
        'King of the Hill CTF; KotH; crown cycle; qualified capture; acquisition; control; reliability; RSCTF',
    },
  },
}

function requestedProfiles() {
  const names = process.argv.length > 2 ? process.argv.slice(2) : ['ad']
  const unknown = names.filter((name) => !Object.hasOwn(PDF_PROFILES, name))
  if (unknown.length > 0) {
    throw new Error(
      `Unknown PDF profile${unknown.length === 1 ? '' : 's'}: ${unknown.join(', ')}. Available profiles: ${Object.keys(PDF_PROFILES).join(', ')}`
    )
  }
  return names.map((name) => PDF_PROFILES[name])
}

const children = new Set()
let shuttingDown = false

const delay = (milliseconds) => new Promise((resolveDelay) => setTimeout(resolveDelay, milliseconds))

function commandWorks(command) {
  const result = spawnSync(command, ['--version'], { stdio: 'ignore' })
  return !result.error && result.status === 0
}

async function findChromium() {
  const configured = process.env.CHROME_BIN?.trim()
  const candidates = [
    configured,
    'chromium',
    'chromium-browser',
    'google-chrome-stable',
    'google-chrome',
    '/snap/bin/chromium',
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    '/Applications/Chromium.app/Contents/MacOS/Chromium',
  ].filter(Boolean)

  for (const candidate of [...new Set(candidates)]) {
    if (isAbsolute(candidate)) {
      try {
        await access(candidate, fsConstants.X_OK)
        return candidate
      } catch {
        continue
      }
    }
    if (commandWorks(candidate)) return candidate
  }

  throw new Error('Chromium was not found. Install it or set CHROME_BIN to its executable path.')
}

async function reservePort() {
  const server = createServer()
  await new Promise((resolveListen, rejectListen) => {
    server.once('error', rejectListen)
    server.listen(0, HOST, resolveListen)
  })
  const address = server.address()
  if (!address || typeof address === 'string') throw new Error('Could not reserve a localhost port')
  await new Promise((resolveClose, rejectClose) =>
    server.close((error) => (error ? rejectClose(error) : resolveClose()))
  )
  return address.port
}

function startChild(command, args, options = {}) {
  const child = spawn(command, args, {
    cwd: DOCS_ROOT,
    env: { ...process.env, ...options.env },
    detached: process.platform !== 'win32',
    stdio: options.quiet ? ['ignore', 'ignore', 'pipe'] : 'inherit',
  })
  child.stderrTail = ''
  if (child.stderr) {
    child.stderr.setEncoding('utf8')
    child.stderr.on('data', (chunk) => {
      child.stderrTail = `${child.stderrTail}${chunk}`.slice(-8_000)
    })
  }
  children.add(child)
  child.once('exit', () => children.delete(child))
  return child
}

async function waitForExit(child) {
  return new Promise((resolveExit, rejectExit) => {
    child.once('error', rejectExit)
    child.once('exit', (code, signal) => {
      if (code === 0) resolveExit()
      else rejectExit(new Error(`Command exited with ${signal ? `signal ${signal}` : `code ${code}`}`))
    })
  })
}

async function terminate(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return

  try {
    if (process.platform === 'win32') {
      spawnSync('taskkill', ['/pid', String(child.pid), '/t', '/f'], {
        stdio: 'ignore',
      })
    } else {
      process.kill(-child.pid, 'SIGTERM')
    }
  } catch {
    child.kill('SIGTERM')
  }

  await Promise.race([
    new Promise((resolveExit) => child.once('exit', resolveExit)),
    delay(2_000).then(() => {
      try {
        if (process.platform !== 'win32') process.kill(-child.pid, 'SIGKILL')
        else child.kill('SIGKILL')
      } catch {
        // The process already exited.
      }
    }),
  ])
}

async function cleanup() {
  if (shuttingDown) return
  shuttingDown = true
  await Promise.allSettled([...children].map(terminate))
}

for (const signal of ['SIGINT', 'SIGTERM']) {
  process.once(signal, async () => {
    await cleanup()
    process.exit(128 + (signal === 'SIGINT' ? 2 : 15))
  })
}

async function waitForHttp(url, child, timeoutMs = START_TIMEOUT_MS) {
  const deadline = Date.now() + timeoutMs
  let lastError

  while (Date.now() < deadline) {
    if (child?.exitCode !== null) throw new Error(`Server exited before ${url} became ready`)
    try {
      const response = await fetch(url, { redirect: 'follow' })
      if (response.ok) return
      lastError = new Error(`HTTP ${response.status}`)
    } catch (error) {
      lastError = error
    }
    await delay(150)
  }

  throw new Error(`Timed out waiting for ${url}: ${lastError?.message ?? 'no response'}`)
}

async function waitForJson(url, child, timeoutMs = START_TIMEOUT_MS) {
  const deadline = Date.now() + timeoutMs
  let lastError

  while (Date.now() < deadline) {
    if (child?.exitCode !== null) throw new Error(`Chromium exited before ${url} became ready`)
    try {
      const response = await fetch(url)
      if (response.ok) return response.json()
      lastError = new Error(`HTTP ${response.status}`)
    } catch (error) {
      lastError = error
    }
    await delay(100)
  }

  const stderr = child?.stderrTail?.trim()
  throw new Error(
    `Timed out waiting for Chromium: ${lastError?.message ?? 'no response'}${stderr ? `\n${stderr}` : ''}`
  )
}

class CdpClient {
  constructor(socket) {
    this.socket = socket
    this.nextId = 0
    this.pending = new Map()

    socket.addEventListener('message', ({ data }) => {
      const message = JSON.parse(data)
      if (!message.id) return
      const pending = this.pending.get(message.id)
      if (!pending) return
      this.pending.delete(message.id)
      clearTimeout(pending.timer)
      if (message.error) pending.reject(new Error(message.error.message))
      else pending.resolve(message.result)
    })

    socket.addEventListener('close', () => {
      for (const pending of this.pending.values()) {
        clearTimeout(pending.timer)
        pending.reject(new Error('Chromium DevTools connection closed'))
      }
      this.pending.clear()
    })
  }

  static async connect(url) {
    const socket = new WebSocket(url)
    await new Promise((resolveOpen, rejectOpen) => {
      socket.addEventListener('open', resolveOpen, { once: true })
      socket.addEventListener('error', () => rejectOpen(new Error('Could not connect to Chromium DevTools')), {
        once: true,
      })
    })
    return new CdpClient(socket)
  }

  send(method, params = {}, timeoutMs = PAGE_TIMEOUT_MS) {
    return new Promise((resolveSend, rejectSend) => {
      const id = ++this.nextId
      const timer = setTimeout(() => {
        this.pending.delete(id)
        rejectSend(new Error(`CDP ${method} timed out`))
      }, timeoutMs)
      this.pending.set(id, { resolve: resolveSend, reject: rejectSend, timer })
      this.socket.send(JSON.stringify({ id, method, params }))
    })
  }

  close() {
    this.socket.close()
  }
}

async function evaluate(client, expression) {
  const { result, exceptionDetails } = await client.send('Runtime.evaluate', {
    expression,
    awaitPromise: true,
    returnByValue: true,
  })
  if (exceptionDetails) {
    throw new Error(exceptionDetails.exception?.description ?? exceptionDetails.text ?? 'Page evaluation failed')
  }
  return result.value
}

async function writePdf(output, pdf) {
  await mkdir(dirname(output), { recursive: true })
  const temporaryOutput = `${output}.${process.pid}.tmp`
  await writeFile(temporaryOutput, pdf)
  try {
    await rename(temporaryOutput, output)
  } catch (error) {
    if (process.platform !== 'win32') throw error
    await rm(output, { force: true })
    await rename(temporaryOutput, output)
  }
}

function pdfHex(value) {
  const bytes = [0xfe, 0xff]
  for (let index = 0; index < value.length; index += 1) {
    const unit = value.charCodeAt(index)
    bytes.push(unit >> 8, unit & 0xff)
  }
  return `<${Buffer.from(bytes).toString('hex').toUpperCase()}>`
}

function pdfDate(date) {
  const part = (value) => String(value).padStart(2, '0')
  return `D:${date.getUTCFullYear()}${part(date.getUTCMonth() + 1)}${part(date.getUTCDate())}${part(date.getUTCHours())}${part(date.getUTCMinutes())}${part(date.getUTCSeconds())}+00'00'`
}

function addPdfMetadata(pdf, metadata) {
  const source = pdf.toString('latin1')
  const startMatches = [...source.matchAll(/startxref\s+(\d+)\s+%%EOF/g)]
  const previousXref = Number(startMatches.at(-1)?.[1])
  const trailer = source.slice(Math.max(0, source.length - 8_000)).match(
    /trailer\s*<<([\s\S]*?)>>\s*startxref\s+\d+\s+%%EOF/
  )?.[1]
  const size = Number(trailer?.match(/\/Size\s+(\d+)/)?.[1])
  const root = trailer?.match(/\/Root\s+(\d+\s+\d+\s+R)/)?.[1]
  if (!Number.isSafeInteger(previousXref) || !Number.isSafeInteger(size) || !root) {
    throw new Error('Could not locate the generated PDF trailer for metadata injection')
  }

  const objectId = size
  const now = pdfDate(new Date())
  const object = `${objectId} 0 obj\n<<\n/Title ${pdfHex(metadata.title)}\n/Author ${pdfHex(metadata.author)}\n/Subject ${pdfHex(metadata.subject)}\n/Keywords ${pdfHex(metadata.keywords)}\n/Creator ${pdfHex('rsctf VitePress journal PDF pipeline')}\n/Producer ${pdfHex('Chromium/Skia PDF with rsctf metadata')}\n/CreationDate (${now})\n/ModDate (${now})\n>>\nendobj\n`
  const prefix = pdf.at(-1) === 0x0a ? '' : '\n'
  const objectOffset = pdf.length + Buffer.byteLength(prefix)
  const xrefOffset = objectOffset + Buffer.byteLength(object)
  const update = `${prefix}${object}xref\n${objectId} 1\n${String(objectOffset).padStart(10, '0')} 00000 n \ntrailer\n<</Size ${objectId + 1}\n/Root ${root}\n/Info ${objectId} 0 R\n/Prev ${previousXref}>>\nstartxref\n${xrefOffset}\n%%EOF\n`
  return Buffer.concat([pdf, Buffer.from(update, 'latin1')])
}

function pageReadyExpression(profile) {
  return `
(async () => {
  const expectedHeading = ${JSON.stringify(profile.expectedHeading)};
  const deadline = performance.now() + ${PAGE_TIMEOUT_MS};
  while (performance.now() < deadline) {
    const heading = document.querySelector('.vp-doc h1');
    const app = document.querySelector('#app');
    if (
      document.readyState === 'complete' &&
      heading?.textContent.includes(expectedHeading) &&
      app?.__vue_app__
    ) break;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }

  const heading = document.querySelector('.vp-doc h1');
  const app = document.querySelector('#app');
  if (!heading || !heading.textContent.includes(expectedHeading) || !app?.__vue_app__) {
    throw new Error('The ' + expectedHeading + ' documentation page did not render');
  }

  await new Promise(requestAnimationFrame);
  await new Promise(requestAnimationFrame);

  const displayMath = [...document.querySelectorAll('.journal-math')];
  if (
    displayMath.length === 0 ||
    displayMath.some((block) => block.querySelector(':scope > math[display="block"]')?.tabIndex !== 0)
  ) {
    throw new Error('Display equations must render as focusable native MathML');
  }
  if (document.querySelector('.vp-doc mjx-container')) {
    throw new Error('All inline and display equations must render as native MathML');
  }
  const inlineMath = [...document.querySelectorAll('.vp-doc math:not([display="block"])')];
  if (inlineMath.length === 0 || inlineMath.some((math) => math.tabIndex >= 0)) {
    throw new Error('Inline equations must render as non-focusable native MathML expressions');
  }

  const tableCaptions = [...document.querySelectorAll('.journal-table-caption')];
  for (const caption of tableCaptions) {
    const table = caption.nextElementSibling;
    if (!(table instanceof HTMLTableElement)) {
      throw new Error('Journal table caption is not followed by a table: ' + caption.textContent);
    }
    const semanticCaption = document.createElement('caption');
    semanticCaption.innerHTML = caption.innerHTML;
    table.prepend(semanticCaption);
    for (const row of table.tBodies[0]?.rows ?? []) {
      const cell = row.cells[0];
      if (!cell || cell.tagName === 'TH') continue;
      const rowHeader = document.createElement('th');
      rowHeader.scope = 'row';
      for (const attribute of cell.attributes) rowHeader.setAttribute(attribute.name, attribute.value);
      rowHeader.innerHTML = cell.innerHTML;
      cell.replaceWith(rowHeader);
    }
    const wrapper = document.createElement('div');
    wrapper.className = ['journal-table', ...caption.classList].filter(
      (name) => name !== 'journal-table-caption'
    ).join(' ');
    caption.replaceWith(wrapper);
    wrapper.append(table);
  }

  const figureCaptions = [...document.querySelectorAll('.journal-figure-caption')];
  for (const caption of figureCaptions) {
    const imageParagraph = caption.previousElementSibling;
    const image = imageParagraph?.querySelector(':scope > img');
    if (!(image instanceof HTMLImageElement)) {
      throw new Error('Journal figure caption is not preceded by an image: ' + caption.textContent);
    }
    const figure = document.createElement('figure');
    figure.className = 'journal-figure';
    const semanticCaption = document.createElement('figcaption');
    semanticCaption.innerHTML = caption.innerHTML;
    figure.append(image, semanticCaption);
    imageParagraph.replaceWith(figure);
    caption.remove();
  }

  let portableLinks = 0;
  const publicBase = ${JSON.stringify(PUBLIC_DOCS_BASE_URL)};
  for (const link of document.querySelectorAll('a[href]')) {
    const raw = link.getAttribute('href');
    if (!raw) continue;
    if (raw.startsWith('#')) {
      portableLinks += 1;
      continue;
    }
    const resolved = new URL(link.href);
    if (resolved.origin !== location.origin) {
      portableLinks += 1;
      continue;
    }
    if (resolved.pathname === location.pathname && resolved.hash) {
      link.setAttribute('href', resolved.hash);
      portableLinks += 1;
      continue;
    }
    if (publicBase) {
      link.href = publicBase + resolved.pathname + resolved.search + resolved.hash;
      portableLinks += 1;
    } else {
      link.removeAttribute('href');
    }
  }

  const images = [...document.images];
  for (const image of images) image.loading = 'eager';
  for (let top = 0; top < document.documentElement.scrollHeight; top += Math.max(innerHeight, 600)) {
    scrollTo(0, top);
    await new Promise(requestAnimationFrame);
  }
  scrollTo(0, 0);

  await Promise.all(images.map((image) => {
    if (image.complete) return Promise.resolve();
    return new Promise((resolve, reject) => {
      image.addEventListener('load', resolve, { once: true });
      image.addEventListener('error', () => reject(new Error('Image failed: ' + image.currentSrc)), { once: true });
    });
  }));
  await document.fonts.ready;

  const broken = images.filter((image) => image.naturalWidth === 0).map((image) => image.currentSrc);
  if (broken.length > 0) throw new Error('Broken images: ' + broken.join(', '));

  await new Promise(requestAnimationFrame);
  await new Promise(requestAnimationFrame);
  const localLinks = [...document.querySelectorAll('a[href]')]
    .map((link) => link.getAttribute('href'))
    .filter((href) => href.includes('127.0.0.1') || href.includes('localhost'));
  if (localLinks.length > 0) throw new Error('Local PDF links remain: ' + localLinks.join(', '));

  return {
    title: heading.textContent.trim(),
    images: images.length,
    figures: figureCaptions.length,
    tables: tableCaptions.length,
    portableLinks,
  };
})()
`
}

async function main() {
  const profiles = requestedProfiles()
  const chromium = await findChromium()
  const previewPort = await reservePort()
  const debugPort = await reservePort()
  const browserProfile = await mkdtemp(join(tmpdir(), 'rsctf-docs-pdf-'))
  let preview
  let browser
  let client

  try {
    console.log('Building VitePress documentation...')
    const docsEnv = { DOCS_BASE: '/', DOCS_HOSTNAME: '' }
    const build = startChild(PNPM, ['exec', 'vitepress', 'build', '.'], {
      env: docsEnv,
    })
    await waitForExit(build)

    preview = startChild(PNPM, ['exec', 'vitepress', 'preview', '.', '--host', HOST, '--port', String(previewPort), '--strictPort'], {
      env: docsEnv,
    })
    for (const profile of profiles) {
      await waitForHttp(`http://${HOST}:${previewPort}${profile.pagePath}`, preview)
    }

    const chromeArgs = [
      '--headless=new',
      '--disable-dev-shm-usage',
      '--disable-gpu',
      '--no-first-run',
      '--no-default-browser-check',
      `--remote-debugging-port=${debugPort}`,
      `--user-data-dir=${browserProfile}`,
      'about:blank',
    ]
    if (typeof process.getuid === 'function' && process.getuid() === 0) chromeArgs.unshift('--no-sandbox')
    browser = startChild(chromium, chromeArgs, { quiet: true })

    const targets = await waitForJson(`http://${HOST}:${debugPort}/json/list`, browser)
    const target = targets.find((candidate) => candidate.type === 'page')
    if (!target?.webSocketDebuggerUrl) throw new Error('Chromium did not expose a page target')

    client = await CdpClient.connect(target.webSocketDebuggerUrl)
    await client.send('Page.enable')
    await client.send('Runtime.enable')
    await client.send('Emulation.setDeviceMetricsOverride', {
      width: 1280,
      height: 900,
      deviceScaleFactor: 1,
      mobile: false,
    })
    for (const profile of profiles) {
      const pageUrl = `http://${HOST}:${previewPort}${profile.pagePath}`
      await client.send('Emulation.setEmulatedMedia', { media: 'screen' })
      await client.send('Page.navigate', { url: pageUrl })
      const page = await evaluate(client, pageReadyExpression(profile))
      await client.send('Emulation.setEmulatedMedia', { media: 'print' })
      await evaluate(
        client,
        `(async () => { await document.fonts.ready; await new Promise(requestAnimationFrame); await new Promise(requestAnimationFrame); return true })()`
      )

      const { data } = await client.send('Page.printToPDF', {
        landscape: false,
        displayHeaderFooter: true,
        headerTemplate:
          `<div style="box-sizing:border-box;width:calc(100% - 30mm);margin:0 15mm;padding:0 0 3px;border-bottom:3px double #000;display:flex;justify-content:space-between;font-family:Times New Roman,serif;font-size:8px;color:#000"><span><b>RSCTF</b>: Technical Practice Paper</span><span>${profile.headerLabel}</span></div>`,
        footerTemplate:
          `<div style="box-sizing:border-box;width:calc(100% - 30mm);margin:0 15mm;padding:3px 0 0;border-top:3px double #000;display:flex;justify-content:space-between;font-family:Times New Roman,serif;font-size:8px;color:#000"><span>Component-specific licensing · <a style="color:#000" href="${LICENSING_URL}">guide</a> · <a style="color:#000" href="${CREEPJS_LICENSE_URL}">CreepJS license</a></span><span>Page <span class="pageNumber"></span> of <span class="totalPages"></span></span></div>`,
        printBackground: true,
        preferCSSPageSize: true,
        paperWidth: 8.2677165354,
        paperHeight: 11.6929133858,
        generateTaggedPDF: true,
        generateDocumentOutline: profile.generateDocumentOutline !== false,
      })
      let pdf = Buffer.from(data, 'base64')
      if (pdf.length < 10_000 || pdf.subarray(0, 5).toString('ascii') !== '%PDF-') {
        throw new Error(`Chromium returned an invalid PDF (${pdf.length} bytes)`)
      }
      pdf = addPdfMetadata(pdf, profile.metadata)
      if (pdf.includes(Buffer.from('127.0.0.1')) || pdf.includes(Buffer.from('localhost'))) {
        throw new Error('Generated PDF contains an ephemeral local link')
      }

      const output = resolve(DOCS_ROOT, 'public/downloads', profile.outputName)
      const distOutput = resolve(DOCS_ROOT, '.vitepress/dist/downloads', profile.outputName)
      await Promise.all([writePdf(output, pdf), writePdf(distOutput, pdf)])

      const written = await readFile(output)
      if (!written.equals(pdf)) throw new Error('PDF verification failed after writing the file')
      console.log(
        `Created ${output} (${pdf.length} bytes, ${page.figures} figures, ${page.tables} tables, ${page.portableLinks} portable links).`
      )
    }
  } finally {
    client?.close()
    await Promise.allSettled([terminate(browser), terminate(preview)])
    await rm(browserProfile, { recursive: true, force: true })
  }
}

main()
  .catch((error) => {
    console.error(error instanceof Error ? error.stack : error)
    process.exitCode = 1
  })
  .finally(cleanup)
