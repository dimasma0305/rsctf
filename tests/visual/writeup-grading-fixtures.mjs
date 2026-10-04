import http from 'node:http'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { fixture as adminFixture } from './admin-navigation-fixtures.mjs'

export function gradingFixture() {
  const board = { generatedAt: Date.now(), fullySettled: true, teams: [
    { participationId: 1, teamId: 1, name: 'Stargazers', divisionId: 1, division: 'Open', writeupUrl: '/assets/writeup-fixture.pdf', originalScore: 80, overallEligible: true, divisionEligible: true,
      challenges: [{ challengeId: 74, title: 'Parcel Pan ic — a deliberately long challenge title for review', mode: 'Jeopardy', earnedPoints: 550, overallPoints: 60, percentage: null, revision: 0 }, { challengeId: 75, title: 'Serpent Circuit', mode: 'KingOfTheHill', earnedPoints: 40, overallPoints: 20, percentage: null, revision: 0 }] },
    { participationId: 2, teamId: 2, name: 'Packet Pioneers', divisionId: 1, division: 'Open', writeupUrl: null, originalScore: 60, overallEligible: true, divisionEligible: true,
      challenges: [{ challengeId: 74, title: 'Parcel Panic', mode: 'Jeopardy', earnedPoints: 500, overallPoints: 60, percentage: 100, revision: 1 }] },
    { participationId: 3, teamId: 3, name: 'Stack Underflow', divisionId: 2, division: 'Invited', writeupUrl: null, originalScore: 10, overallEligible: false, divisionEligible: true, challenges: [{ challengeId: 75, title: 'Serpent Circuit', mode: 'KingOfTheHill', earnedPoints: 20, overallPoints: 10, percentage: null, revision: 0 }] },
    { participationId: 4, teamId: 4, name: 'No solves yet', divisionId: 1, division: 'Open', writeupUrl: null, originalScore: 0, overallEligible: true, divisionEligible: true, challenges: [] },
  ] }
  return (path, method = 'GET', body = '') => {
    const p = new URL(path, 'http://localhost').pathname
    if (p === '/api/admin/writeups/19/grading') return { body: board }
    const match = p.match(/^\/api\/admin\/writeups\/19\/grading\/(\d+)\/(\d+)$/)
    if (match && method === 'PUT') {
      const model = JSON.parse(body)
      const cell = board.teams.find((t) => t.participationId === Number(match[1]))?.challenges.find((c) => c.challengeId === Number(match[2]))
      if (!cell || cell.revision !== model.expectedRevision) return { status: 409, body: { title: 'Refresh before saving again' } }
      if (model.percentage !== null && (!Number.isInteger(model.percentage) || model.percentage < 0 || model.percentage > 100)) return { status: 400, body: { title: 'Invalid grade' } }
      cell.percentage = model.percentage
      cell.revision++
      return { body: { participationId: Number(match[1]), challengeId: cell.challengeId, percentage: cell.percentage, revision: cell.revision } }
    }
    return adminFixture(path, method)
  }
}

export function pdfFixture({ pages = 3, height = 220 } = {}) {
  if (!Number.isInteger(pages) || pages < 1 || pages > 100) throw new Error('Fixture requires 1–100 pages')
  const objects = [
    '<< /Type /Catalog /Pages 2 0 R >>', `<< /Type /Pages /Kids [${Array.from({ length: pages }, (_, i) => `${4 + i * 2} 0 R`).join(' ')}] /Count ${pages} >>`,
    '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
  ]
  for (let page = 1; page <= pages; page++) {
    const stream = `BT /F1 18 Tf 30 160 Td (Writeup review fixture - page ${page}) Tj ET`
    objects.push(
      `<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 ${height}] /Resources << /Font << /F1 3 0 R >> >> /Contents ${objects.length + 2} 0 R >>`,
      `<< /Length ${stream.length} >>\nstream\n${stream}\nendstream`,
    )
  }
  let pdf = '%PDF-1.4\n', offsets = [0]
  for (const [i, object] of objects.entries()) { offsets.push(pdf.length); pdf += `${i + 1} 0 obj\n${object}\nendobj\n` }
  const xref = pdf.length
  pdf += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n${offsets.slice(1).map((o) => String(o).padStart(10, '0') + ' 00000 n ').join('\n')}\ntrailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`
  return Buffer.from(pdf)
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url) && process.argv.includes('--serve')) {
  const fixture = gradingFixture()
  http.createServer(async (req, res) => {
    try {
      if (!['GET', 'HEAD'].includes(req.method)) { res.writeHead(405); return res.end() }
      if (req.url === '/assets/writeup-fixture.pdf') { res.writeHead(200, { 'Content-Type': 'application/pdf' }); return res.end(pdfFixture()) }
      if (req.url.toLowerCase().startsWith('/api/')) {
        const r = fixture(req.url, req.method)
        res.writeHead(r.status || 200, { 'Content-Type': 'application/json' }); return res.end(JSON.stringify(r.body))
      }
      const response = await fetch('http://127.0.0.1:63017' + req.url)
      res.writeHead(response.status, { 'Content-Type': response.headers.get('Content-Type') || 'application/octet-stream' })
      res.end(Buffer.from(await response.arrayBuffer()))
    } catch { res.writeHead(502); res.end('Fixture proxy failed') }
  }).listen(63018, '127.0.0.1')
}
