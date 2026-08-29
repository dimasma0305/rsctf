import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const source = readFileSync(new URL('../applib.mjs', import.meta.url), 'utf8');

test('lifecycle organizer helpers satisfy revisioned mutation contracts', () => {
  const createGame = source.slice(
    source.indexOf('export async function createGame'),
    source.indexOf('export async function setGameSchedule')
  );
  const schedule = source.slice(
    source.indexOf('export async function setGameSchedule'),
    source.indexOf('export async function createChallenge')
  );
  const createChallenge = source.slice(
    source.indexOf('export async function createChallenge'),
    source.indexOf('export async function setChallenge')
  );
  const updateChallenge = source.slice(
    source.indexOf('export async function setChallenge'),
    source.indexOf('/** Rebuild one exact challenge')
  );
  const addFlags = source.slice(
    source.indexOf('export async function addFlags'),
    source.indexOf('export async function deleteGame')
  );

  assert.match(createGame, /operationId: randomUUID\(\)/);
  assert.match(schedule, /operationId: randomUUID\(\)/);
  assert.match(createChallenge, /operationId: randomUUID\(\)/);
  assert.match(updateChallenge, /GET.*\/challenges\/\$\{cid\}/s);
  assert.match(updateChallenge, /expectedRevision: current\.revision/);
  assert.match(updateChallenge, /operationId: randomUUID\(\)/);
  assert.match(addFlags, /operationId: randomUUID\(\)/);
  assert.match(addFlags, /flags: flags\.map/);
});
