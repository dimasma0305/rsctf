import assert from 'node:assert/strict'

import {
  CHECK_STATUS,
  FORMULAS,
  SCORERS,
  addCapture,
  adjudicateEpochStatus,
  aggregateEpochs,
  assertConserved,
  cloneRound,
  exclusiveSweep,
  makeRound,
  scoreActiveMatrix,
  scoreBounded,
  scoreBoundedScarcity,
  scoreCoverageTransfer,
  scoreCurrent,
  scoreFlatTransfer,
  scoreIctf,
  scoreMaturityPositiveDefense,
  scoreMaturityScarcity,
  scoreNormalizedCoverage,
  scoreSqrtTransfer,
  scoreManualArithmetic,
  scoreManualArithmeticEpoch,
  scoreManualBalanced,
  scoreManualBalancedEpoch,
  singleFlag,
  summarizeEpochTotals,
  tickCredit,
  universalCapture,
} from './lib.mjs'

function qualificationContext(teamCount, victim, attackers) {
  const context = Array.from({ length: teamCount }, () => new Set())
  context[victim] = new Set(attackers)
  return context
}

function fullQualificationContext(teamCount) {
  return Array.from({ length: teamCount }, (_, victim) => new Set(
    Array.from({ length: teamCount }, (__, attacker) => attacker)
      .filter((attacker) => attacker !== victim),
  ))
}

function assertScoresClose(actual, expected, epsilon = 1e-12) {
  assert.equal(actual.length, expected.length)
  for (let team = 0; team < actual.length; team += 1) {
    for (const component of ['attack', 'defense', 'quality', 'total']) {
      assert(Math.abs(actual[team][component] - expected[team][component]) < epsilon)
    }
  }
}

for (const scorer of [
  scoreCurrent,
  scoreSqrtTransfer,
  scoreNormalizedCoverage,
  scoreBoundedScarcity,
  scoreMaturityScarcity,
]) {
  assertConserved(scorer(exclusiveSweep(20)))
  assertConserved(scorer(universalCapture(20)))
}

for (const teamCount of [5, 20, 300]) {
  const sweep = exclusiveSweep(teamCount)
  assert(Math.abs(scoreCurrent(sweep)[0].attack - (teamCount - 1)) < 1e-12)
  assert(Math.abs(scoreSqrtTransfer(sweep)[0].attack - Math.sqrt(teamCount - 1)) < 1e-12)
  assert(Math.abs(scoreFlatTransfer(sweep)[0].attack - 1) < 1e-12)
  assert(Math.abs(scoreNormalizedCoverage(sweep)[0].attack - 0.45) < 1e-12)
  assert.equal(scoreNormalizedCoverage(sweep)[0].quality, 1)
  const scarcity = scoreBoundedScarcity(sweep)[0]
  const expectedScarcitySweep = 0.45 * (2 - 1 / (teamCount - 1))
  assert(Math.abs(scarcity.attack - expectedScarcitySweep) < 1e-12)
  assert(scarcity.attack < scarcity.quality)

  const maturityContext = fullQualificationContext(teamCount)
  const maturityScores = scoreMaturityScarcity(sweep, {
    qualifiedAttackersByVictim: maturityContext,
  })
  const active = teamCount - 1
  const u = Math.max(0, Math.min(1, (active - 4) / 4))
  const maturityWeight = u * u * (3 - 2 * u)
  const expectedMaturitySweep = 0.45 * (1 + maturityWeight * (1 - 1 / active))
  assert(Math.abs(maturityScores[0].attack - expectedMaturitySweep) < 1e-12)
  assert(maturityScores[0].attack < maturityScores[0].quality)
  assertConserved(maturityScores)

  const positiveDefenseScores = scoreMaturityPositiveDefense(sweep, {
    qualifiedAttackersByVictim: maturityContext,
  })
  const expectedPositiveSweep = 0.30 * (1 + maturityWeight * (1 - 1 / active))
  assert(Math.abs(positiveDefenseScores[0].attack - expectedPositiveSweep) < 1e-12)
  assert(positiveDefenseScores.every((score) => score.defense === 0))
  assert(positiveDefenseScores.every((score) => (
    score.attack >= 0 && score.defense >= 0 && score.quality >= 0 && score.total >= 0
  )))
  const positiveIssuance = positiveDefenseScores.reduce(
    (sum, score) => sum + score.attack + score.defense,
    0,
  )
  const equivalentMaturityAttack = maturityScores.reduce(
    (sum, score) => sum + score.attack,
    0,
  )
  assert(positiveIssuance <= equivalentMaturityAttack + 1e-12)
}

const currentOne = scoreCurrent(singleFlag(20, 1))
const currentTwo = scoreCurrent(singleFlag(20, 2))
assert.equal(currentOne[1].attack, 1)
assert.equal(currentTwo[1].attack, 0.5)
assert.equal(currentOne[0].defense, -1)

const sqrtOne = scoreSqrtTransfer(singleFlag(20, 1))
const sqrtAll = scoreSqrtTransfer(singleFlag(20, 19))
assert(Math.abs(sqrtOne[1].attack - 1 / Math.sqrt(19)) < 1e-12)
assert(Math.abs(sqrtAll[0].defense + 1) < 1e-12)

const duplicate = makeRound(8)
addCapture(duplicate, 1, 0)
addCapture(duplicate, 1, 0)
assert.equal(scoreCurrent(duplicate)[1].attack, 1)

const captureRound = makeRound(5)
addCapture(captureRound, 2, 0)
const captureClone = cloneRound(captureRound)
addCapture(captureClone, 3, 0)
assert(!captureRound.captures[0].has(3))

const ictf = scoreIctf(universalCapture(8))
assert(Math.abs(ictf.reduce((sum, score) => sum + score.total, 0) - 8) < 1e-12)

const matrixRound = exclusiveSweep(8)
matrixRound.health[1] = 0
const matrix = scoreActiveMatrix(matrixRound)
assert(Math.abs(matrix[0].attack - Math.sqrt(8)) < 1e-12)
assert.equal(matrix[1].defense, 0)
assert.equal(matrix[1].quality, 0)

const downCapture = makeRound(8)
downCapture.health[1] = 0
addCapture(downCapture, 0, 1)
const downCaptureScore = scoreActiveMatrix(downCapture)
assert.equal(downCaptureScore[0].attack, Math.sqrt(8) / 7)
assert.equal(downCaptureScore[1].defense, 0)
assert.equal(downCaptureScore[1].quality, 0)

const matrixUniqueControl = makeRound(8)
addCapture(matrixUniqueControl, 1, 2)
const matrixUniqueAttacked = singleFlag(8, 1)
const matrixUniqueDamage = scoreActiveMatrix(matrixUniqueControl)[0].defense
  - scoreActiveMatrix(matrixUniqueAttacked)[0].defense
const matrixFieldControl = makeRound(8)
for (let attacker = 1; attacker < 8; attacker += 1) {
  addCapture(matrixFieldControl, attacker, attacker === 7 ? 1 : attacker + 1)
}
const matrixFieldDamage = scoreActiveMatrix(matrixFieldControl)[0].defense
  - scoreActiveMatrix(singleFlag(8, 7))[0].defense
assert(Math.abs(matrixUniqueDamage - Math.sqrt(8) / 7) < 1e-12)
assert(Math.abs(matrixFieldDamage / matrixUniqueDamage - 7) < 1e-12)

for (const score of scoreBounded(universalCapture(20))) {
  assert(score.total >= 0 && score.total <= 100)
}

const offenseHeavy = scoreBounded(exclusiveSweep(20), {
  attackWeight: 0.45,
  defenseWeight: 0.30,
  qualityWeight: 0.25,
})
assert.equal(offenseHeavy[0].attack, 45)
assert.equal(offenseHeavy[0].defense, 30)
assert.equal(offenseHeavy[0].quality, 25)
assert.equal(offenseHeavy[0].total, 100)

for (const teamCount of [5, 20, 300]) {
  const opponents = teamCount - 1
  const unique = singleFlag(teamCount, 1)
  const copied = singleFlag(teamCount, opponents)

  const currentUnique = scoreCurrent(unique)
  const currentCopied = scoreCurrent(copied)
  assert(Math.abs(currentCopied[1].attack / currentUnique[1].attack - 1 / opponents) < 1e-12)
  assert.equal(currentUnique[0].defense, -1)
  assert.equal(currentCopied[0].defense, -1)

  const sqrtUnique = scoreSqrtTransfer(unique)
  const sqrtCopied = scoreSqrtTransfer(copied)
  assert(Math.abs(sqrtCopied[1].attack / sqrtUnique[1].attack - 1 / Math.sqrt(opponents)) < 1e-12)
  assert(Math.abs(sqrtUnique[0].defense + 1 / Math.sqrt(opponents)) < 1e-12)
  assert.equal(sqrtCopied[0].defense, -1)

  const boundedUnique = scoreBounded(unique)
  const boundedCopied = scoreBounded(copied)
  assert.equal(boundedCopied[1].attack, boundedUnique[1].attack)
  assert(boundedCopied[0].defense < boundedUnique[0].defense)
  assert(Math.abs(
    (35 - boundedUnique[0].defense) / boundedUnique[1].attack - 0.35 / 0.4,
  ) < 1e-12)

  const normalizedUnique = scoreNormalizedCoverage(unique)
  const normalizedCopied = scoreNormalizedCoverage(copied)
  assert.equal(normalizedUnique[1].attack, normalizedCopied[1].attack)
  assert(Math.abs(normalizedUnique[1].attack + normalizedUnique[0].defense) < 1e-12)
  assert(Math.abs(normalizedCopied[0].defense / normalizedUnique[0].defense - opponents) < 1e-10)

  const recoveringCompromised = scoreNormalizedCoverage(singleFlag(teamCount, opponents, 0.5))[0]
  const downUncaptured = scoreNormalizedCoverage(makeRound(teamCount, 0))[0]
  assert(recoveringCompromised.total > downUncaptured.total)
}

const scarcityEmpty = scoreBoundedScarcity(singleFlag(20, 0))
assert(scarcityEmpty.every((score) => score.attack === 0 && score.defense === 0))
assert(scarcityEmpty.every((score) => score.quality === 1 && score.total === 1))

const scarcityOpponents = 19
const scarcityPool = 0.45
const scarcityOneShare = (scarcityPool / scarcityOpponents) * (2 - 1 / scarcityOpponents)
const scarcityTwoShare = (scarcityPool / scarcityOpponents) * (2 - 2 / scarcityOpponents)
const scarcityOne = scoreBoundedScarcity(singleFlag(20, 1))
const scarcityTwo = scoreBoundedScarcity(singleFlag(20, 2))
const scarcityFull = scoreBoundedScarcity(singleFlag(20, scarcityOpponents))
assert(Math.abs(scarcityOne[1].attack - scarcityOneShare) < 1e-12)
assert(Math.abs(scarcityOne[0].defense + scarcityOneShare) < 1e-12)
assert(Math.abs(scarcityTwo[1].attack - scarcityTwoShare) < 1e-12)
assert(Math.abs(scarcityTwo[2].attack - scarcityTwoShare) < 1e-12)
assert(Math.abs(scarcityTwo[0].defense + 2 * scarcityTwoShare) < 1e-12)
assert(scarcityOne[1].attack > scarcityTwo[1].attack)
assert(Math.abs(scarcityFull[1].attack - scarcityPool / scarcityOpponents) < 1e-12)
assert(Math.abs(scarcityFull[0].defense + scarcityPool) < 1e-12)

for (let capturers = 0; capturers <= scarcityOpponents; capturers += 1) {
  const scores = scoreBoundedScarcity(singleFlag(20, capturers))
  const attack = scores.reduce((sum, score) => sum + score.attack, 0)
  assertConserved(scores)
  assert(attack >= 0 && attack <= scarcityPool + 1e-12)
  assert(scores[0].defense <= 0 && scores[0].defense >= -scarcityPool - 1e-12)
}

const customScarcity = scoreBoundedScarcity(singleFlag(5, 1), {
  transferWeight: 0.2,
  slaWeight: 0.5,
})
assert(Math.abs(customScarcity[1].attack - 0.2 / 4 * (2 - 1 / 4)) < 1e-12)
assert.equal(customScarcity[1].quality, 0.5)

for (const round of [
  singleFlag(20, 0),
  singleFlag(20, 2),
  exclusiveSweep(20),
  universalCapture(20),
]) {
  assertScoresClose(scoreMaturityScarcity(round), scoreNormalizedCoverage(round))
}

const maturityBase = 0.45 / 19
const maturityAtFour = scoreMaturityScarcity(singleFlag(20, 1), {
  qualifiedAttackersByVictim: qualificationContext(20, 0, [1, 2, 3, 4]),
})
assert(Math.abs(maturityAtFour[1].attack - maturityBase) < 1e-12)
const maturityAtFourCopied = scoreMaturityScarcity(singleFlag(20, 3), {
  qualifiedAttackersByVictim: qualificationContext(20, 0, [1, 2, 3, 4]),
})
assert(Math.abs(maturityAtFourCopied[1].attack - maturityBase) < 1e-12)

const maturityAtSix = scoreMaturityScarcity(singleFlag(20, 1), {
  qualifiedAttackersByVictim: qualificationContext(20, 0, [1, 2, 3, 4, 5, 6]),
})
const maturityHalfShare = maturityBase * (1 + 0.5 * (1 - 1 / 6))
assert(Math.abs(maturityAtSix[1].attack - maturityHalfShare) < 1e-12)
const maturityAtSixCopied = scoreMaturityScarcity(singleFlag(20, 2), {
  qualifiedAttackersByVictim: qualificationContext(20, 0, [3, 4, 5, 6]),
})
assert(Math.abs(maturityAtSixCopied[1].attack - maturityBase * (1 + 0.5 * (1 - 2 / 6))) < 1e-12)

const maturityAtEight = scoreMaturityScarcity(singleFlag(20, 1), {
  qualifiedAttackersByVictim: qualificationContext(20, 0, [1, 2, 3, 4, 5, 6, 7, 8]),
})
assert(Math.abs(maturityAtEight[1].attack - maturityBase * (2 - 1 / 8)) < 1e-12)
const maturityAboveEight = scoreMaturityScarcity(singleFlag(20, 1), {
  qualifiedAttackersByVictim: qualificationContext(20, 0, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]),
})
assert(Math.abs(maturityAboveEight[1].attack - maturityBase * (2 - 1 / 10)) < 1e-12)

for (let active = 1; active <= 19; active += 1) {
  for (let capturers = 1; capturers <= active; capturers += 1) {
    const scores = scoreMaturityScarcity(singleFlag(20, capturers), {
      qualifiedAttackersByVictim: qualificationContext(
        20,
        0,
        Array.from({ length: active }, (_, index) => index + 1),
      ),
    })
    const attack = scores.reduce((sum, score) => sum + score.attack, 0)
    assertConserved(scores)
    assert(attack >= 0 && attack <= 0.45 + 1e-12)
    assert(scores[0].defense <= 0 && scores[0].defense >= -0.45 - 1e-12)
  }
}

const recoveringMaturity = scoreMaturityScarcity(singleFlag(20, 19, 0.5), {
  qualifiedAttackersByVictim: fullQualificationContext(20),
})[0]
const downMaturity = scoreMaturityScarcity(makeRound(20, 0))[0]
assert(recoveringMaturity.total > downMaturity.total)

const customMaturity = scoreMaturityScarcity(singleFlag(20, 1), {
  transferWeight: 0.2,
  slaWeight: 0.5,
  qualifiedAttackersByVictim: qualificationContext(20, 0, [1, 2, 3, 4, 5, 6]),
})
assert(Math.abs(customMaturity[1].attack - (0.2 / 19) * (1 + 0.5 * (1 - 1 / 6))) < 1e-12)
assert.equal(customMaturity[1].quality, 0.5)

const positiveNoAttack = scoreMaturityPositiveDefense(makeRound(20), {
  qualifiedAttackersByVictim: fullQualificationContext(20),
})
assert(positiveNoAttack.every((score) => score.attack === 0 && score.defense === 0))
assert(positiveNoAttack.every((score) => score.quality === 1 && score.total === 1))

const positiveMissedRound = makeRound(4, [1, 1, 0, 0.5])
addCapture(positiveMissedRound, 0, 1)
const positiveMissed = scoreMaturityPositiveDefense(positiveMissedRound, {
  qualifiedAttackersByVictim: qualificationContext(4, 1, [0]),
})
const positiveMissedAttack = 0.30 / 3
assert(Math.abs(positiveMissed[0].attack - positiveMissedAttack) < 1e-12)
assert.equal(positiveMissed[2].defense, 0)
assert(Math.abs(positiveMissed[3].defense - 0.5 * 0.5 * positiveMissedAttack) < 1e-12)
assert.equal(positiveMissed[3].quality, 0.5)

const positiveHealthyMissedRound = cloneRound(positiveMissedRound)
positiveHealthyMissedRound.health[3] = 1
const positiveHealthyMissed = scoreMaturityPositiveDefense(positiveHealthyMissedRound)
assert(Math.abs(positiveHealthyMissed[3].defense - 0.5 * positiveMissedAttack) < 1e-12)
assert(Math.abs(positiveMissed[3].defense - 0.5 * positiveHealthyMissed[3].defense) < 1e-12)

const positiveFullSweep = scoreMaturityPositiveDefense(exclusiveSweep(20), {
  qualifiedAttackersByVictim: fullQualificationContext(20),
})
assert(positiveFullSweep.every((score) => score.defense === 0))

const positiveIssuanceRound = singleFlag(20, 3)
const positiveIssuanceContext = fullQualificationContext(20)
const positiveIssuanceScores = scoreMaturityPositiveDefense(positiveIssuanceRound, {
  qualifiedAttackersByVictim: positiveIssuanceContext,
})
const positiveAttackTotal = positiveIssuanceScores.reduce(
  (sum, score) => sum + score.attack,
  0,
)
const positiveDefenseTotal = positiveIssuanceScores.reduce(
  (sum, score) => sum + score.defense,
  0,
)
const equivalentMaturityScores = scoreMaturityScarcity(positiveIssuanceRound, {
  transferWeight: 0.45,
  qualifiedAttackersByVictim: positiveIssuanceContext,
})
const equivalentMaturityAttack = equivalentMaturityScores.reduce(
  (sum, score) => sum + score.attack,
  0,
)
assert(positiveDefenseTotal <= 0.5 * positiveAttackTotal + 1e-12)
assert(positiveAttackTotal + positiveDefenseTotal <= equivalentMaturityAttack + 1e-12)
assert(positiveIssuanceScores.every((score) => (
  score.attack >= 0 && score.defense >= 0 && score.quality >= 0 && score.total >= 0
)))

const positiveBase = 0.30 / 19
for (const active of [1, 4, 5, 6, 7, 8, 19]) {
  const progress = Math.max(0, Math.min(1, (active - 4) / 4))
  const weight = progress * progress * (3 - 2 * progress)
  const scores = scoreMaturityPositiveDefense(singleFlag(20, 1), {
    qualifiedAttackersByVictim: qualificationContext(
      20,
      0,
      Array.from({ length: active }, (_, index) => index + 1),
    ),
  })
  const expected = positiveBase * (1 + weight * (1 - 1 / active))
  assert(Math.abs(scores[1].attack - expected) < 1e-12)
}

function manualVectorRound(teamCount, capturedTargets, incomingCaptures) {
  const round = makeRound(teamCount)
  for (let victim = 1; victim <= capturedTargets; victim += 1) {
    // Full-field captures suppress rarity so vector tests isolate A, D, and Core.
    for (let attacker = 0; attacker < teamCount; attacker += 1) {
      if (attacker !== victim) addCapture(round, attacker, victim)
    }
  }
  for (let attacker = 1; attacker <= incomingCaptures; attacker += 1) {
    addCapture(round, attacker, 0)
  }
  return round
}

const manualSilence = makeRound(5)
const arithmeticZeroOne = scoreManualArithmetic(manualSilence)[0]
const balancedZeroOne = scoreManualBalanced(manualSilence)[0]
assert.equal(arithmeticZeroOne.attackRate, 0)
assert.equal(arithmeticZeroOne.defenseRate, 1)
assert.equal(arithmeticZeroOne.total, 50)
assert.equal(balancedZeroOne.total, 40)

const attackOnlyRound = manualVectorRound(5, 4, 4)
const arithmeticOneZero = scoreManualArithmetic(attackOnlyRound)[0]
const balancedOneZero = scoreManualBalanced(attackOnlyRound)[0]
assert.equal(arithmeticOneZero.attackRate, 1)
assert.equal(arithmeticOneZero.defenseRate, 0)
assert.equal(arithmeticOneZero.total, arithmeticZeroOne.total)
assert.equal(balancedOneZero.total, balancedZeroOne.total)
assert.equal(balancedOneZero.balance, 0)

const perfectManualRound = manualVectorRound(5, 4, 0)
const perfectArithmetic = scoreManualArithmetic(perfectManualRound)[0]
const perfectBalanced = scoreManualBalanced(perfectManualRound)[0]
assert.equal(perfectArithmetic.total, 100)
assert.equal(perfectBalanced.total, 100)
assert.equal(perfectBalanced.balance, 20)

const quarterManualRound = manualVectorRound(5, 1, 3)
const quarterArithmetic = scoreManualArithmetic(quarterManualRound)[0]
const quarterBalanced = scoreManualBalanced(quarterManualRound)[0]
assert.equal(quarterArithmetic.attackRate, 0.25)
assert.equal(quarterArithmetic.defenseRate, 0.25)
assert.equal(quarterArithmetic.total, 25)
assert.equal(quarterBalanced.total, 25)

const oneQuarterManualRound = manualVectorRound(5, 4, 3)
assert.equal(scoreManualArithmetic(oneQuarterManualRound)[0].total, 62.5)
assert.equal(scoreManualBalanced(oneQuarterManualRound)[0].total, 60)

const skewManualRound = manualVectorRound(6, 4, 4)
assert.equal(scoreManualArithmetic(skewManualRound)[0].total, 50)
assert(Math.abs(scoreManualBalanced(skewManualRound)[0].total - 48) < 1e-12)

const recoveringManualRound = cloneRound(perfectManualRound)
recoveringManualRound.health[0] = 0.5
const recoveringManual = scoreManualBalanced(recoveringManualRound)[0]
assert.equal(recoveringManual.attackRate, 1)
assert.equal(recoveringManual.defenseRate, 1)
assert.equal(recoveringManual.slaMultiplier, 0.5)
assert.equal(recoveringManual.total, 0.5 * perfectBalanced.total)
const downManualRound = cloneRound(perfectManualRound)
downManualRound.health[0] = 0
const downManual = scoreManualBalanced(downManualRound)[0]
assert.equal(downManual.defenseOpportunities, 0)
assert.equal(downManual.defenseRate, 0)
assert.equal(downManual.total, 0)

// Epoch scoring aggregates evidence and raw SLA before applying Core and R.
// Averaging the already-completed tick scores would produce 50 here, which is
// not equivalent to the documented epoch formula.
const perfectEpochTick = manualVectorRound(3, 2, 0)
const downEpochTick = makeRound(3)
downEpochTick.health[0] = 0
const tickArithmeticMean = (
  scoreManualArithmetic(perfectEpochTick)[0].total
  + scoreManualArithmetic(downEpochTick)[0].total
) / 2
const tickBalancedMean = (
  scoreManualBalanced(perfectEpochTick)[0].total
  + scoreManualBalanced(downEpochTick)[0].total
) / 2
assert.equal(tickArithmeticMean, 50)
assert.equal(tickBalancedMean, 50)

const epochArithmetic = scoreManualArithmeticEpoch([perfectEpochTick, downEpochTick])[0]
const epochBalanced = scoreManualBalancedEpoch([perfectEpochTick, downEpochTick])[0]
assert.equal(epochArithmetic.acceptedCaptures, 2)
assert.equal(epochArithmetic.attackOpportunities, 4)
assert.equal(epochArithmetic.captureCoverage, 0.5)
assert.equal(epochArithmetic.defenseOpportunities, 2)
assert.equal(epochArithmetic.protectedOpportunities, 2)
assert.equal(epochArithmetic.defenseRate, 1)
assert.equal(epochArithmetic.rawSlaCredit, 1)
assert.equal(epochArithmetic.eligibleServiceTicks, 2)
assert.equal(epochArithmetic.slaMultiplier, 0.5)
assert.equal(epochArithmetic.total, 37.5)
assert(Math.abs(epochBalanced.total - 37.071067811865476) < 1e-12)
assert.notEqual(epochArithmetic.total, tickArithmeticMean)
assert.notEqual(epochBalanced.total, tickBalancedMean)

const preStartRound = manualVectorRound(5, 0, 4)
const postStartRound = manualVectorRound(5, 4, 0)
const startRoundScore = scoreManualBalancedEpoch([preStartRound, postStartRound], {
  startRound: 2,
})[0]
assert.equal(startRoundScore.startRound, 2)
assert.equal(startRoundScore.inputRoundCount, 2)
assert.equal(startRoundScore.scoredRoundCount, 1)
assert.equal(startRoundScore.total, scoreManualBalanced(postStartRound)[0].total)

const rareRound = makeRound(6)
addCapture(rareRound, 0, 1)
const rareScore = scoreManualArithmetic(rareRound)[0]
assert.equal(rareScore.captureCoverage, 0.2)
assert(Math.abs(rareScore.rarityFractionSum - 0.8) < 1e-12)
assert(Math.abs(rareScore.rarityRate - 0.16) < 1e-12)
assert(Math.abs(rareScore.rarityPremium - 0.04) < 1e-12)
assert(Math.abs(rareScore.attackRate - 0.24) < 1e-12)

const rareEpoch = scoreManualArithmeticEpoch([rareRound, makeRound(6)])[0]
assert.equal(rareEpoch.captureCoverage, 0.1)
assert(Math.abs(rareEpoch.rarityRate - 0.08) < 1e-12)
assert(Math.abs(rareEpoch.rarityPremium - 0.02) < 1e-12)
assert(Math.abs(rareEpoch.attackRate - 0.12) < 1e-12)

const copiedRareRound = cloneRound(rareRound)
addCapture(copiedRareRound, 2, 1)
addCapture(copiedRareRound, 3, 1)
const copiedRare = scoreManualArithmetic(copiedRareRound)[0]
assert(Math.abs(copiedRare.rarityFractionSum - 0.4) < 1e-12)
assert(Math.abs(copiedRare.rarityPremium - 0.02) < 1e-12)
assert(Math.abs(copiedRare.attackRate - 0.22) < 1e-12)

const smallFieldRareRound = makeRound(4)
addCapture(smallFieldRareRound, 0, 1)
const smallFieldRare = scoreManualArithmetic(smallFieldRareRound)[0]
assert.equal(smallFieldRare.rarityRate, 0)
assert.equal(smallFieldRare.rarityPremium, 0)
assert.equal(smallFieldRare.attackRate, 1 / 3)

const pairwiseBypass = scoreManualBalanced(singleFlag(20, 1))[0]
assert.equal(pairwiseBypass.defenseOpportunities, 19)
assert.equal(pairwiseBypass.protectedOpportunities, 18)
assert.equal(pairwiseBypass.defenseRate, 18 / 19)
assert(scoreManualBalanced(singleFlag(20, 19))[0].defenseRate === 0)

const mixedDefenseEpoch = scoreManualBalancedEpoch([
  singleFlag(20, 1),
  singleFlag(20, 19),
])[0]
assert.equal(mixedDefenseEpoch.defenseOpportunities, 38)
assert.equal(mixedDefenseEpoch.protectedOpportunities, 18)
assert.equal(mixedDefenseEpoch.defenseRate, 9 / 19)

const ineligibleFlag = singleFlag(5, 1)
ineligibleFlag.health[0] = 0
const ineligibleOwner = scoreManualBalanced(ineligibleFlag)[0]
const ineligibleAttacker = scoreManualBalanced(ineligibleFlag)[1]
assert.equal(ineligibleOwner.defenseOpportunities, 0)
assert.equal(ineligibleAttacker.attackOpportunities, 4)
assert.equal(ineligibleAttacker.acceptedCaptures, 1)

// An accepted frozen-roster capture qualifies offense reachability even when
// the victim's checker is both unhealthy and unable to verify the planted flag.
// The resulting flag is one common opportunity for every frozen opponent, but
// it still creates no defense opportunity for the owner.
const captureQualifiedRound = makeRound(5, [0, 1, 1, 1, 1], false)
addCapture(captureQualifiedRound, 1, 0)
const captureQualifiedScores = scoreManualBalanced(captureQualifiedRound)
assert.equal(captureQualifiedScores[0].defenseOpportunities, 0)
assert.equal(captureQualifiedScores[1].attackOpportunities, 1)
assert.equal(captureQualifiedScores[1].acceptedCaptures, 1)
assert.equal(captureQualifiedScores[1].attackRate, 1)
for (let team = 2; team < 5; team += 1) {
  assert.equal(captureQualifiedScores[team].attackOpportunities, 1)
  assert.equal(captureQualifiedScores[team].acceptedCaptures, 0)
}

const fallbackTcpRound = makeRound(5, 1, false)
const fallbackTcpScore = scoreManualBalanced(fallbackTcpRound)[0]
assert.equal(fallbackTcpScore.slaMultiplier, 1)
assert.equal(fallbackTcpScore.attackOpportunities, 0)
assert.equal(fallbackTcpScore.defenseOpportunities, 0)
assert.equal(fallbackTcpScore.total, 0)

const raritySweep = scoreManualArithmetic(exclusiveSweep(20, 0))[0]
assert.equal(raritySweep.rarityPremium, 0)
assert.equal(raritySweep.attackRate, 1)

const nearMaximumRarityRound = makeRound(101)
for (let victim = 1; victim <= 80; victim += 1) {
  addCapture(nearMaximumRarityRound, 0, victim)
}
const nearMaximumRarity = scoreManualArithmetic(nearMaximumRarityRound)[0]
assert(nearMaximumRarity.rarityPremium > 0.19)
assert(
  nearMaximumRarity.rarityPremium
    <= 0.25 * nearMaximumRarity.captureCoverage + 1e-12,
)
assert(nearMaximumRarity.rarityPremium <= 0.20)

for (const scorer of [scoreManualArithmetic, scoreManualBalanced]) {
  for (const round of [
    manualSilence,
    perfectManualRound,
    quarterManualRound,
    skewManualRound,
    rareRound,
  ]) {
    const scores = scorer(round)
    assert(scores.every((score) => (
      score.attack >= 0
        && score.defense >= 0
        && score.quality >= 0
        && score.balance >= 0
        && score.total >= 0
        && score.attackRate >= 0
        && score.attackRate <= 1
        && score.defenseRate >= 0
        && score.defenseRate <= 1
        && score.rarityPremium >= 0
        && score.rarityPremium <= 0.25 * score.captureCoverage + 1e-12
        && score.rarityPremium <= 0.20 + 1e-12
        && score.total <= score.recipientBudget + 1e-12
    )))
  }
}

const serviceWeights = [0.8, 1, 1.2]
const weightedScores = serviceWeights.map((serviceWeight) =>
  scoreManualBalanced(perfectManualRound, {
    epochBudget: 100,
    serviceWeight,
    totalServiceWeight: 3,
  })[0].total,
)
assert(Math.abs(weightedScores.reduce((sum, score) => sum + score, 0) - 100) < 1e-12)

assert.throws(() => scoreBounded(makeRound(8), {
  attackWeight: -0.1,
  defenseWeight: 0.6,
  qualityWeight: 0.5,
}), /finite and nonnegative/)
assert.throws(() => scoreBounded(makeRound(8), {
  attackWeight: Number.NaN,
  defenseWeight: 0.75,
  qualityWeight: 0.25,
}), /finite and nonnegative/)
assert.throws(() => scoreBounded(makeRound(8), {
  attackWeight: Number.POSITIVE_INFINITY,
  defenseWeight: Number.NEGATIVE_INFINITY,
  qualityWeight: 1,
}), /finite and nonnegative/)
const boundedDown = exclusiveSweep(8)
boundedDown.health[0] = 0
assert.equal(scoreBounded(boundedDown)[0].defense, 0)
assert.equal(scoreBounded(boundedDown)[0].quality, 0)

const changed = cloneRound(singleFlag(8, 1))
addCapture(changed, 2, 0)
assert(scoreCurrent(changed)[1].attack < scoreCurrent(singleFlag(8, 1))[1].attack)

assert.equal(aggregateEpochs([10, 20, 30], 'equal'), 20)
assert(aggregateEpochs([100, 0, 100, 100], 'moderate-recency') > 75)
assert.throws(() => aggregateEpochs([1], 'unknown'))
assert.deepEqual(summarizeEpochTotals([80, 20], {
  finalizedEpochCount: 1,
  configuredTicksPerEpoch: 8,
  epochTickCounts: [8, 1],
}), {
  settledTotal: 80,
  projectedTotal: 73.33333333333333,
  finalizedEpochCount: 1,
  epochCount: 2,
  configuredTicksPerEpoch: 8,
  epochTickCounts: [8, 1],
  epochWeights: [1, 0.125],
})
assert.deepEqual(summarizeEpochTotals([80, 20], {
  finalizedEpochCount: 1,
  gameEnded: true,
  configuredTicksPerEpoch: 8,
  epochTickCounts: [8, 1],
}), {
  settledTotal: 73.33333333333333,
  projectedTotal: 73.33333333333333,
  finalizedEpochCount: 2,
  epochCount: 2,
  configuredTicksPerEpoch: 8,
  epochTickCounts: [8, 1],
  epochWeights: [1, 0.125],
})
assert.throws(() => summarizeEpochTotals([10], { finalizedEpochCount: 2 }))
assert.throws(() => summarizeEpochTotals([10, 20], {
  configuredTicksPerEpoch: 8,
  epochTickCounts: [4, 8],
}), /only the final epoch/)
assert.throws(() => scoreCoverageTransfer(singleFlag(5, 1), { gamma: -0.1 }))
assert.throws(() => scoreCoverageTransfer(singleFlag(5, 1), { gamma: Number.NaN }))
assert.throws(() => scoreCoverageTransfer(singleFlag(5, 1), { pool: 0 }))
assert.throws(() => scoreNormalizedCoverage(singleFlag(5, 1), { transferWeight: 0 }))
assert.throws(() => scoreNormalizedCoverage(singleFlag(5, 1), { transferWeight: Number.NaN }))
assert.throws(() => scoreNormalizedCoverage(singleFlag(5, 1), {
  transferWeight: 0.5,
  slaWeight: 1,
}))
assert.throws(() => scoreBoundedScarcity(singleFlag(5, 1), { transferWeight: 0 }))
assert.throws(() => scoreBoundedScarcity(singleFlag(5, 1), {
  transferWeight: Number.NaN,
}))
assert.throws(() => scoreBoundedScarcity(singleFlag(5, 1), {
  transferWeight: 0.5,
  slaWeight: 1,
}))
assert.throws(() => scoreBoundedScarcity(singleFlag(5, 1), {
  slaWeight: Number.POSITIVE_INFINITY,
}))
const invalidScarcityRound = singleFlag(5, 4)
invalidScarcityRound.captures[0].add(0)
assert.throws(() => scoreBoundedScarcity(invalidScarcityRound), /capturer count/)
const selfCapturedScarcityRound = makeRound(5)
selfCapturedScarcityRound.captures[0].add(0)
assert.throws(() => scoreBoundedScarcity(selfCapturedScarcityRound), /eligible opponent/)
assert.throws(() => scoreMaturityScarcity(singleFlag(5, 1), { transferWeight: 0 }))
assert.throws(() => scoreMaturityScarcity(singleFlag(5, 1), {
  transferWeight: Number.NaN,
}))
assert.throws(() => scoreMaturityScarcity(singleFlag(5, 1), {
  transferWeight: 0.5,
  slaWeight: 1,
}))
assert.throws(() => scoreMaturityScarcity(singleFlag(5, 1), {
  slaWeight: Number.POSITIVE_INFINITY,
}))
assert.throws(() => scoreMaturityPositiveDefense(singleFlag(5, 1), { attackWeight: 0 }))
assert.throws(() => scoreMaturityPositiveDefense(singleFlag(5, 1), {
  attackWeight: Number.NaN,
}))
assert.throws(() => scoreMaturityPositiveDefense(singleFlag(5, 1), {
  attackWeight: 0.5,
  slaWeight: 1,
}))
assert.throws(() => scoreMaturityPositiveDefense(singleFlag(5, 1), {
  slaWeight: Number.POSITIVE_INFINITY,
}))
assert.throws(() => scoreMaturityPositiveDefense(singleFlag(5, 1), {
  defenseRatio: -0.1,
}), /in \[0, 1\]/)
assert.throws(() => scoreMaturityPositiveDefense(singleFlag(5, 1), {
  defenseRatio: 1.1,
}), /in \[0, 1\]/)
assert.throws(() => scoreMaturityPositiveDefense(singleFlag(5, 1), {
  defenseRatio: Number.NaN,
}), /in \[0, 1\]/)
assert.throws(() => scoreMaturityPositiveDefense(singleFlag(5, 1), {
  defenseRatio: Number.POSITIVE_INFINITY,
}), /in \[0, 1\]/)
assert.throws(() => scoreMaturityScarcity(singleFlag(5, 1), { rampStart: -1 }))
assert.throws(() => scoreMaturityScarcity(singleFlag(5, 1), { rampStart: 1.5 }))
assert.throws(() => scoreMaturityScarcity(singleFlag(5, 1), {
  rampStart: 4,
  rampEnd: 4,
}))
assert.throws(() => scoreMaturityScarcity(singleFlag(5, 1), {
  rampStart: 4,
  rampEnd: Number.POSITIVE_INFINITY,
}))
assert.throws(() => scoreMaturityScarcity(singleFlag(5, 1), {
  qualifiedAttackersByVictim: [],
}), /match teamCount/)
assert.throws(() => scoreMaturityPositiveDefense(singleFlag(5, 1), { rampStart: -1 }))
assert.throws(() => scoreMaturityPositiveDefense(singleFlag(5, 1), { rampStart: 1.5 }))
assert.throws(() => scoreMaturityPositiveDefense(singleFlag(5, 1), {
  rampStart: 4,
  rampEnd: 4,
}))
assert.throws(() => scoreMaturityPositiveDefense(singleFlag(5, 1), {
  rampStart: 4,
  rampEnd: Number.POSITIVE_INFINITY,
}))
assert.throws(() => scoreMaturityPositiveDefense(singleFlag(5, 1), {
  qualifiedAttackersByVictim: [],
}), /match teamCount/)
const invalidMaturityEntry = Array.from({ length: 5 }, () => new Set())
invalidMaturityEntry[0] = []
assert.throws(() => scoreMaturityScarcity(singleFlag(5, 1), {
  qualifiedAttackersByVictim: invalidMaturityEntry,
}), /must be a Set/)
assert.throws(() => scoreMaturityPositiveDefense(singleFlag(5, 1), {
  qualifiedAttackersByVictim: invalidMaturityEntry,
}), /must be a Set/)
const victimMaturityContext = Array.from({ length: 5 }, () => new Set())
victimMaturityContext[0].add(0)
assert.throws(() => scoreMaturityScarcity(singleFlag(5, 1), {
  qualifiedAttackersByVictim: victimMaturityContext,
}), /eligible opponent/)
assert.throws(() => scoreMaturityPositiveDefense(singleFlag(5, 1), {
  qualifiedAttackersByVictim: victimMaturityContext,
}), /eligible opponent/)
const outOfRangeMaturityContext = Array.from({ length: 5 }, () => new Set())
outOfRangeMaturityContext[0].add(5)
assert.throws(() => scoreMaturityScarcity(singleFlag(5, 1), {
  qualifiedAttackersByVictim: outOfRangeMaturityContext,
}), /eligible opponent/)
assert.throws(() => scoreMaturityPositiveDefense(singleFlag(5, 1), {
  qualifiedAttackersByVictim: outOfRangeMaturityContext,
}), /eligible opponent/)
const invalidMaturityRound = singleFlag(5, 4)
invalidMaturityRound.captures[0].add(0)
assert.throws(() => scoreMaturityScarcity(invalidMaturityRound), /capturer count/)
assert.throws(() => scoreMaturityPositiveDefense(invalidMaturityRound), /capturer count/)
const selfCapturedMaturityRound = makeRound(5)
selfCapturedMaturityRound.captures[0].add(0)
assert.throws(() => scoreMaturityScarcity(selfCapturedMaturityRound), /eligible opponent/)
assert.throws(() => scoreMaturityPositiveDefense(selfCapturedMaturityRound), /eligible opponent/)
const missingCapturesRound = makeRound(5)
delete missingCapturesRound.captures
assert.throws(() => scoreManualArithmetic(missingCapturesRound), /captures must match/)
const invalidCapturesEntry = makeRound(5)
invalidCapturesEntry.captures[0] = []
assert.throws(() => scoreManualArithmetic(invalidCapturesEntry), /must be a Set/)
const selfCapturedManualRound = makeRound(5)
selfCapturedManualRound.captures[0].add(0)
assert.throws(() => scoreManualArithmetic(selfCapturedManualRound), /eligible opponent/)
const outOfRangeManualRound = makeRound(5)
outOfRangeManualRound.captures[0].add(5)
assert.throws(() => scoreManualArithmetic(outOfRangeManualRound), /eligible opponent/)
assert.throws(
  () => scoreManualBalancedEpoch([makeRound(5), makeRound(6)]),
  /stable teamCount/,
)
for (const options of [
  { epochBudget: 0 },
  { epochBudget: Number.NaN },
  { serviceWeight: 0.7 },
  { serviceWeight: 1.3 },
  { serviceWeight: Number.NaN },
  { serviceWeight: 1.2, totalServiceWeight: 1.1 },
  { totalServiceWeight: Number.POSITIVE_INFINITY },
  { rarityCoefficient: -0.1 },
  { rarityCoefficient: 0.3 },
  { rarityCoefficient: Number.NaN },
  { rarityMinOpponents: 0 },
  { rarityMinOpponents: 1.5 },
  { startRound: 0 },
  { startRound: 2 },
  { startRound: 1.5 },
]) {
  assert.throws(() => scoreManualArithmetic(makeRound(5), options))
  assert.throws(() => scoreManualBalanced(makeRound(5), options))
}
assert.equal(tickCredit(CHECK_STATUS.ok, null), 1)
assert.equal(tickCredit(CHECK_STATUS.ok, CHECK_STATUS.ok), 1)
assert.equal(tickCredit(CHECK_STATUS.ok, CHECK_STATUS.offline), 0.5)
assert.equal(tickCredit(CHECK_STATUS.ok, CHECK_STATUS.mumble), 0.5)
assert.equal(tickCredit(CHECK_STATUS.ok, CHECK_STATUS.internalError), 1)
assert.equal(tickCredit(CHECK_STATUS.offline, CHECK_STATUS.ok), 0)
assert.equal(tickCredit(CHECK_STATUS.mumble, CHECK_STATUS.ok), 0)
assert.equal(tickCredit(CHECK_STATUS.internalError, CHECK_STATUS.ok), 0)
assert.throws(() => tickCredit('invalid', CHECK_STATUS.ok))
const epochOffline = adjudicateEpochStatus(CHECK_STATUS.offline)
const epochFault = adjudicateEpochStatus(CHECK_STATUS.internalError, epochOffline)
const epochRecovered = adjudicateEpochStatus(CHECK_STATUS.ok, epochFault)
assert.equal(epochFault.credit, 0)
assert.equal(epochFault.effectiveStatus, CHECK_STATUS.offline)
assert.equal(epochFault.carried, true)
assert.equal(epochRecovered.credit, 0.5)
assert.deepEqual(adjudicateEpochStatus(CHECK_STATUS.internalError), {
  credit: null,
  effectiveStatus: null,
  carried: false,
  voidServiceTick: true,
})
const epochInitialFault = adjudicateEpochStatus(CHECK_STATUS.internalError)
assert.equal(adjudicateEpochStatus(CHECK_STATUS.ok, epochInitialFault).credit, 1)
assert.equal(SCORERS[FORMULAS.scarcity], scoreBoundedScarcity)
assert.equal(SCORERS[FORMULAS.maturity], scoreMaturityScarcity)
assert.equal(SCORERS[FORMULAS.positiveDefense], scoreMaturityPositiveDefense)
assert.equal(SCORERS[FORMULAS.manualArithmetic], scoreManualArithmetic)
assert.equal(SCORERS[FORMULAS.manualBalanced], scoreManualBalanced)
assert.equal(Object.keys(SCORERS).length, Object.keys(FORMULAS).length)

console.log('ad scoring simulator tests: ok')
