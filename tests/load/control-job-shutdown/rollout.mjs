// Real durable rollout execution; worker observations are injected in PostgreSQL.
// This isolates control-process shutdown from the separate native-worker gate.
import assert from 'node:assert/strict'
import { randomUUID, createHash } from 'node:crypto'

const hash = value => createHash('sha256').update(value).digest('hex')
const literal = value => "'" + String(value).replaceAll("'", "''") + "'"
export async function rolloutShutdown({ sql, start, enqueue, until, jobState, signalWhileActive }) {
  for (const force of [false, true, 'deadline']) {
    const worker = randomUUID(), workload = randomUUID(), assignment = randomUUID(), container = randomUUID()
    const spec = replicas => ({
      gameKind: 'jeopardy', platform: { operatingSystem: 'linux', architecture: 'amd64' },
      services: [{ name: 'challenge', image: { type: 'registryDigest', repository: 'fixture.invalid/challenge', digest: 'sha256:' + 'a'.repeat(64) },
        resources: { cpuMillis: 100, memoryBytes: 1048576, storageBytes: 536870912 },
        replicas, stateless: true, environment: {}, ports: [{ name: 'http', containerPort: 8080, protocol: 'tcp' }] }],
      primaryEndpoint: { service: 'challenge', port: 'http' }, flagTarget: { service: 'challenge', path: '/flag' },
    })
    const current = spec(1), target = spec(2)
    await sql(`INSERT INTO "WorkerNodes" (id,name,platform_os,platform_architecture,runtime_kind,capabilities,
      capacity_cpu_millis,capacity_memory_bytes,capacity_slots)
      VALUES ('${worker}','shutdown-${worker}','linux','amd64','docker','{"maxWorkloadReplicas":512}',10000,1073741824,10);
      INSERT INTO "WorkerWorkloads" (id,owner_kind,owner_key,worker_id,assignment_id,generation,spec_hash_sha256,spec,
        required_os,required_architecture,required_runtime,reserved_cpu_millis,reserved_memory_bytes,reserved_slots,required_replicas,observed_state)
      VALUES ('${workload}','shutdown','${workload}','${worker}','${assignment}',1,decode('${hash(JSON.stringify(current))}','hex'),
        ${literal(JSON.stringify(current))},'linux','amd64','docker',100,1048576,1,1,'Ready');
      INSERT INTO "Containers" (id,image,container_id,status,started_at,expect_stop_at,is_proxy,ip,port)
      VALUES ('${container}','fixture','rsctf-worker:${workload}:${assignment}:1',1,now(),now()+interval '1 hour',true,'',8080);
      UPDATE "GameChallenges" SET workload_spec=${literal(JSON.stringify(target))},shared_container_id='${container}',
        enable_shared_container=true,build_status=4 WHERE id=1`)
    await start()
    const fingerprint = hash(JSON.stringify('workload:sha256:' + hash(JSON.stringify(target))))
    const job = await enqueue('WorkloadRollout', 1, { challengeId: 1 }, fingerprint)
    await until(async () => {
      const row = await jobState(job)
      assert.notEqual(row.status, 3, row.error)
      return await sql(`SELECT container_id FROM "Containers" WHERE id='${container}'`) === `rsctf-worker:${workload}:${assignment}:2`
    }, 'rollout generation published while waiting for worker readiness')
    assert.equal((await jobState(job)).status, 1)
    await signalWhileActive('WorkloadRollout', job, force,
      () => sql(`UPDATE "WorkerWorkloads" SET observed_state='Ready',observed_at=now(),ready_at=now()
        WHERE id='${workload}' AND assignment_id='${assignment}' AND generation=2`),
      async () => {
        assert.equal(await sql(`SELECT generation FROM "WorkerWorkloads" WHERE id='${workload}'`), '2', 'restart must not advance the workload again')
        const result = (await jobState(job)).result
        assert.equal(result.matched, 1)
        assert.equal(result.updated + result.alreadyCurrent, 1)
        for (const field of ['stale', 'incompatible', 'insufficientCapacity', 'failed']) assert.equal(result[field], 0)
      })
    await sql(`UPDATE "GameChallenges" SET shared_container_id=NULL WHERE id=1;
      DELETE FROM "Containers" WHERE id='${container}'; DELETE FROM "WorkerWorkloads" WHERE id='${workload}';
      DELETE FROM "WorkerNodes" WHERE id='${worker}'`)
  }
}
