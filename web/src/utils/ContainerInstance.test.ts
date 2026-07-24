import assert from 'node:assert/strict'
import test from 'node:test'
import { containerOwnerLabel, hasContainerProxy } from './ContainerInstance'

const labels = {
  shared: 'Shared (all teams)',
  adminTest: 'Admin test',
  exercise: 'Exercise',
  unassigned: 'Unassigned',
}

test('container ownership prefers a concrete team', () => {
  assert.equal(
    containerOwnerLabel(
      {
        team: { name: 'red' },
        ownerKind: 'Shared',
      },
      labels
    ),
    'red'
  )
})

test('container ownership describes shared, test, exercise, and unknown rows', () => {
  assert.equal(containerOwnerLabel({ ownerKind: 'Shared' }, labels), 'Shared (all teams)')
  assert.equal(containerOwnerLabel({ ownerKind: 'AdminTest' }, labels), 'Admin test')
  assert.equal(containerOwnerLabel({ ownerKind: 'Exercise', ownerName: 'alice' }, labels), 'Exercise: alice')
  assert.equal(containerOwnerLabel({}, labels), 'Unassigned')
})

test('only proxy-enabled container UUIDs produce a proxy action', () => {
  assert.equal(hasContainerProxy({ containerGuid: 'container-id', isProxy: true }), true)
  assert.equal(hasContainerProxy({ containerGuid: 'container-id', isProxy: false }), false)
  assert.equal(hasContainerProxy({ containerGuid: undefined, isProxy: true }), false)
})
