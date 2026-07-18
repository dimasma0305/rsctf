import assert from 'node:assert/strict'

export const FORMULAS = Object.freeze({
  current: 'rsctf-legacy-gamma-0',
  sqrt: 'sqrt-transfer-gamma-0.5',
  flat: 'flat-transfer-gamma-1',
  normalized: 'normalized-coverage-conserved',
  scarcity: 'bounded-scarcity-conserved',
  maturity: 'maturity-gated-scarcity-conserved',
  positiveDefense: 'maturity-positive-defense',
  manualArithmetic: 'manual-equal-arithmetic',
  manualBalanced: 'manual-equal-balanced',
  ictf: 'ictf-fixed-pot',
  matrix: 'active-attacker-matrix',
  bounded: 'bounded-coverage-normalized',
})

export const CHECK_STATUS = Object.freeze({
  ok: 'ok',
  offline: 'offline',
  mumble: 'mumble',
  internalError: 'internal-error',
})

// Mirrors services/ad/engine/reducers.rs::tick_credit. An OK tick immediately
// after Offline/Mumble is recovering; InternalError does not trigger recovery.
export function tickCredit(current, previous = null) {
  assert(Object.values(CHECK_STATUS).includes(current), 'unknown current check status')
  assert(
    previous === null || Object.values(CHECK_STATUS).includes(previous),
    'unknown previous check status',
  )
  if (current !== CHECK_STATUS.ok) return 0
  return previous === CHECK_STATUS.offline || previous === CHECK_STATUS.mumble ? 0.5 : 1
}

// Official policy for infrastructure faults. Preserve the last non-infra
// checker state/credit across InternalError; without a prior sample, signal the
// orchestrator to void this challenge-round SLA sample across the frozen roster.
export function adjudicateEpochStatus(current, previous = null) {
  assert(Object.values(CHECK_STATUS).includes(current), 'unknown current check status')
  if (previous !== null) {
    const settled = Object.values(CHECK_STATUS).includes(previous.effectiveStatus)
      && Number.isFinite(previous.credit)
      && previous.credit >= 0
      && previous.credit <= 1
    const voided = previous.effectiveStatus === null
      && previous.credit === null
      && previous.voidServiceTick === true
    assert(settled || voided, 'invalid previous adjudication')
  }
  if (current === CHECK_STATUS.internalError) {
    if (previous === null || previous.effectiveStatus === null) {
      return { credit: null, effectiveStatus: null, carried: false, voidServiceTick: true }
    }
    return { ...previous, carried: true, voidServiceTick: false }
  }
  return {
    credit: tickCredit(current, previous?.effectiveStatus ?? null),
    effectiveStatus: current,
    carried: false,
    voidServiceTick: false,
  }
}

export function makeRound(teamCount, health = 1, flagEligibility = null) {
  assert(Number.isInteger(teamCount) && teamCount >= 2, 'teamCount must be >= 2')
  const values = Array.isArray(health)
    ? [...health]
    : Array.from({ length: teamCount }, () => health)
  assert.equal(values.length, teamCount, 'health length must equal teamCount')
  for (const value of values) {
    assert(Number.isFinite(value) && value >= 0 && value <= 1, 'health must be in [0, 1]')
  }
  const eligibleFlags = flagEligibility === null
    ? values.map((value) => value > 0)
    : Array.isArray(flagEligibility)
      ? [...flagEligibility]
      : Array.from({ length: teamCount }, () => flagEligibility)
  assert.equal(eligibleFlags.length, teamCount, 'flagEligibility length must equal teamCount')
  assert(eligibleFlags.every((value) => typeof value === 'boolean'), 'flagEligibility must be boolean')
  return {
    teamCount,
    health: values,
    flagEligible: eligibleFlags,
    captures: Array.from({ length: teamCount }, () => new Set()),
  }
}

export function cloneRound(round) {
  const copy = makeRound(round.teamCount, round.health, round.flagEligible)
  copy.captures = round.captures.map((capturers) => new Set(capturers))
  return copy
}

export function addCapture(round, attacker, victim) {
  assert(Number.isInteger(attacker) && attacker >= 0 && attacker < round.teamCount)
  assert(Number.isInteger(victim) && victim >= 0 && victim < round.teamCount)
  assert.notEqual(attacker, victim, 'self captures are invalid')
  round.captures[victim].add(attacker)
  return round
}

export function exclusiveSweep(teamCount, attacker = 0, health = 1) {
  const round = makeRound(teamCount, health)
  for (let victim = 0; victim < teamCount; victim += 1) {
    if (victim !== attacker) addCapture(round, attacker, victim)
  }
  return round
}

export function universalCapture(teamCount, health = 1) {
  const round = makeRound(teamCount, health)
  for (let victim = 0; victim < teamCount; victim += 1) {
    for (let attacker = 0; attacker < teamCount; attacker += 1) {
      if (attacker !== victim) addCapture(round, attacker, victim)
    }
  }
  return round
}

export function singleFlag(teamCount, capturerCount, health = 1) {
  assert(capturerCount >= 0 && capturerCount < teamCount)
  const round = makeRound(teamCount, health)
  for (let attacker = 1; attacker <= capturerCount; attacker += 1) {
    addCapture(round, attacker, 0)
  }
  return round
}

function blankScores(teamCount) {
  return Array.from({ length: teamCount }, (_, team) => ({
    team,
    attack: 0,
    defense: 0,
    quality: 0,
    total: 0,
  }))
}

function finish(scores) {
  for (const score of scores) {
    score.total = score.attack + score.defense + score.quality + (score.balance ?? 0)
  }
  return scores
}

function addQuality(scores, round, pool) {
  const fieldScale = pool * Math.sqrt(round.teamCount)
  for (let team = 0; team < round.teamCount; team += 1) {
    scores[team].quality += fieldScale * round.health[team]
  }
}

// Legacy coverage-transfer family. gamma=0 is the retired fixed rarity pot,
// gamma=0.5 is its square-root control, and gamma=1 removes rarity entirely.
export function scoreCoverageTransfer(round, { pool = 1, gamma = 0.5 } = {}) {
  assert(Number.isFinite(pool) && pool > 0, 'pool must be finite and positive')
  assert(Number.isFinite(gamma) && gamma >= 0 && gamma <= 1, 'gamma must be in [0, 1]')
  const scores = blankScores(round.teamCount)
  const eligibleOpponents = round.teamCount - 1
  addQuality(scores, round, pool)

  for (let victim = 0; victim < round.teamCount; victim += 1) {
    const capturers = round.captures[victim]
    const count = capturers.size
    if (count === 0) continue
    const transfer = pool * (count / eligibleOpponents) ** gamma
    const share = transfer / count
    scores[victim].defense -= transfer
    for (const attacker of capturers) scores[attacker].attack += share
  }
  return finish(scores)
}

export function scoreCurrent(round, options = {}) {
  return scoreCoverageTransfer(round, { ...options, gamma: 0 })
}

export function scoreSqrtTransfer(round, options = {}) {
  return scoreCoverageTransfer(round, { ...options, gamma: 0.5 })
}

export function scoreFlatTransfer(round, options = {}) {
  return scoreCoverageTransfer(round, { ...options, gamma: 1 })
}

// No-rarity coverage score with field-normalized components. Each accepted
// capture transfers transferWeight/M; SLA contributes slaWeight*q. Requiring
// SLA > 2*transfer keeps even a recovering, fully compromised service better
// than shutdown while attack/defense remain exactly conserved.
export function scoreNormalizedCoverage(
  round,
  { transferWeight = 0.45, slaWeight = 1 } = {},
) {
  assert(
    Number.isFinite(transferWeight) && transferWeight > 0,
    'transferWeight must be finite and positive',
  )
  assert(
    Number.isFinite(slaWeight) && slaWeight > 2 * transferWeight,
    'slaWeight must exceed twice transferWeight',
  )
  const scores = blankScores(round.teamCount)
  const opponents = round.teamCount - 1
  const captureValue = transferWeight / opponents

  for (let team = 0; team < round.teamCount; team += 1) {
    scores[team].quality = slaWeight * round.health[team]
  }
  for (let victim = 0; victim < round.teamCount; victim += 1) {
    for (const attacker of round.captures[victim]) {
      scores[attacker].attack += captureValue
      scores[victim].defense -= captureValue
    }
  }
  return finish(scores)
}

// Beta=1 scarcity bonus over normalized coverage. A flag with k eligible
// capturers pays each attacker (P/M)*(2-k/M); the victim loses the identical
// aggregate transfer. The concave transfer k*share is monotone and capped at P.
export function scoreBoundedScarcity(
  round,
  { transferWeight = 0.45, slaWeight = 1 } = {},
) {
  assert(
    Number.isFinite(transferWeight) && transferWeight > 0,
    'transferWeight must be finite and positive',
  )
  assert(
    Number.isFinite(slaWeight) && slaWeight > 2 * transferWeight,
    'slaWeight must exceed twice transferWeight',
  )
  const scores = blankScores(round.teamCount)
  const opponents = round.teamCount - 1

  for (let team = 0; team < round.teamCount; team += 1) {
    scores[team].quality = slaWeight * round.health[team]
  }
  for (let victim = 0; victim < round.teamCount; victim += 1) {
    const capturers = round.captures[victim]
    const count = capturers.size
    assert(count <= opponents, 'capturer count cannot exceed eligible opponents')
    if (count === 0) continue
    for (const attacker of capturers) {
      assert(
        Number.isInteger(attacker)
          && attacker >= 0
          && attacker < round.teamCount
          && attacker !== victim,
        'capturer must be an eligible opponent',
      )
    }

    const share = (transferWeight / opponents) * (2 - count / opponents)
    const transfer = count * share
    scores[victim].defense -= transfer
    for (const attacker of capturers) scores[attacker].attack += share
  }
  return finish(scores)
}

// Scarcity bonus gated by the victim-relative active-attacker population. The
// smoothstep ramp prevents a small early field from immediately receiving the
// full bonus; without qualification context, A=k and this is normalized coverage.
export function scoreMaturityScarcity(
  round,
  {
    transferWeight = 0.45,
    slaWeight = 1,
    qualifiedAttackersByVictim = null,
    rampStart = 4,
    rampEnd = 8,
  } = {},
) {
  assert(
    Number.isFinite(transferWeight) && transferWeight > 0,
    'transferWeight must be finite and positive',
  )
  assert(
    Number.isFinite(slaWeight) && slaWeight > 2 * transferWeight,
    'slaWeight must exceed twice transferWeight',
  )
  assert(
    Number.isInteger(rampStart) && rampStart >= 0,
    'rampStart must be a nonnegative integer',
  )
  assert(
    Number.isInteger(rampEnd) && rampEnd > rampStart,
    'rampEnd must be an integer greater than rampStart',
  )
  if (qualifiedAttackersByVictim !== null) {
    assert(
      Array.isArray(qualifiedAttackersByVictim)
        && qualifiedAttackersByVictim.length === round.teamCount,
      'qualifiedAttackersByVictim must match teamCount',
    )
    for (let victim = 0; victim < round.teamCount; victim += 1) {
      const qualified = qualifiedAttackersByVictim[victim]
      assert(qualified instanceof Set, 'each qualification context entry must be a Set')
      for (const attacker of qualified) {
        assert(
          Number.isInteger(attacker)
            && attacker >= 0
            && attacker < round.teamCount
            && attacker !== victim,
          'qualified attacker must be an eligible opponent',
        )
      }
    }
  }

  const scores = blankScores(round.teamCount)
  const opponents = round.teamCount - 1
  for (let team = 0; team < round.teamCount; team += 1) {
    scores[team].quality = slaWeight * round.health[team]
  }

  for (let victim = 0; victim < round.teamCount; victim += 1) {
    const capturers = round.captures[victim]
    const count = capturers.size
    assert(count <= opponents, 'capturer count cannot exceed eligible opponents')
    for (const attacker of capturers) {
      assert(
        Number.isInteger(attacker)
          && attacker >= 0
          && attacker < round.teamCount
          && attacker !== victim,
        'capturer must be an eligible opponent',
      )
    }
    if (count === 0) continue

    const active = new Set(capturers)
    if (qualifiedAttackersByVictim !== null) {
      for (const attacker of qualifiedAttackersByVictim[victim]) active.add(attacker)
    }
    const activeCount = active.size
    assert(
      count <= activeCount && activeCount <= opponents,
      'capturers must be a subset of eligible active attackers',
    )

    const u = Math.max(0, Math.min(
      1,
      (activeCount - rampStart) / (rampEnd - rampStart),
    ))
    const maturity = u * u * (3 - 2 * u)
    const share = (transferWeight / opponents)
      * (1 + maturity * (1 - count / activeCount))
    const transfer = count * share
    scores[victim].defense -= transfer
    for (const attacker of capturers) scores[attacker].attack += share
  }
  return finish(scores)
}

// Maturity-gated attack rewards without victim debits. An attacker also funds
// a bounded positive-defense pool for healthy targets it did not capture.
export function scoreMaturityPositiveDefense(
  round,
  {
    attackWeight = 0.30,
    slaWeight = 1,
    defenseRatio = 0.5,
    qualifiedAttackersByVictim = null,
    rampStart = 4,
    rampEnd = 8,
  } = {},
) {
  assert(
    Number.isFinite(attackWeight) && attackWeight > 0,
    'attackWeight must be finite and positive',
  )
  assert(
    Number.isFinite(slaWeight) && slaWeight > 2 * attackWeight,
    'slaWeight must exceed twice attackWeight',
  )
  assert(
    Number.isFinite(defenseRatio) && defenseRatio >= 0 && defenseRatio <= 1,
    'defenseRatio must be finite and in [0, 1]',
  )
  assert(
    Number.isInteger(rampStart) && rampStart >= 0,
    'rampStart must be a nonnegative integer',
  )
  assert(
    Number.isInteger(rampEnd) && rampEnd > rampStart,
    'rampEnd must be an integer greater than rampStart',
  )
  if (qualifiedAttackersByVictim !== null) {
    assert(
      Array.isArray(qualifiedAttackersByVictim)
        && qualifiedAttackersByVictim.length === round.teamCount,
      'qualifiedAttackersByVictim must match teamCount',
    )
    for (let victim = 0; victim < round.teamCount; victim += 1) {
      const qualified = qualifiedAttackersByVictim[victim]
      assert(qualified instanceof Set, 'each qualification context entry must be a Set')
      for (const attacker of qualified) {
        assert(
          Number.isInteger(attacker)
            && attacker >= 0
            && attacker < round.teamCount
            && attacker !== victim,
          'qualified attacker must be an eligible opponent',
        )
      }
    }
  }

  const scores = blankScores(round.teamCount)
  const opponents = round.teamCount - 1
  for (let team = 0; team < round.teamCount; team += 1) {
    scores[team].quality = slaWeight * round.health[team]
  }

  for (let victim = 0; victim < round.teamCount; victim += 1) {
    const capturers = round.captures[victim]
    const count = capturers.size
    assert(count <= opponents, 'capturer count cannot exceed eligible opponents')
    for (const attacker of capturers) {
      assert(
        Number.isInteger(attacker)
          && attacker >= 0
          && attacker < round.teamCount
          && attacker !== victim,
        'capturer must be an eligible opponent',
      )
    }
    if (count === 0) continue

    const active = new Set(capturers)
    if (qualifiedAttackersByVictim !== null) {
      for (const attacker of qualifiedAttackersByVictim[victim]) active.add(attacker)
    }
    const activeCount = active.size
    assert(
      count <= activeCount && activeCount <= opponents,
      'capturers must be a subset of eligible active attackers',
    )

    const u = Math.max(0, Math.min(
      1,
      (activeCount - rampStart) / (rampEnd - rampStart),
    ))
    const maturity = u * u * (3 - 2 * u)
    const share = (attackWeight / opponents)
      * (1 + maturity * (1 - count / activeCount))
    for (const attacker of capturers) scores[attacker].attack += share
  }

  for (let attacker = 0; attacker < round.teamCount; attacker += 1) {
    const attack = scores[attacker].attack
    if (attack <= 0) continue
    const missed = []
    for (let victim = 0; victim < round.teamCount; victim += 1) {
      if (
        victim !== attacker
        && !round.captures[victim].has(attacker)
        && round.health[victim] > 0
      ) {
        missed.push(victim)
      }
    }
    if (missed.length === 0) continue

    const share = defenseRatio * attack / missed.length
    for (const victim of missed) scores[victim].defense += round.health[victim] * share
  }
  return finish(scores)
}

// Manual outcome scoring settles one service at the epoch boundary. The fixed
// teamCount models the roster frozen at startRound. An exact healthy custom
// check or any accepted frozen-roster capture makes a flag offense-eligible for
// every frozen opponent; only the former creates pairwise defense opportunities.
// Aggregate evidence before applying Core and local SLA because averaging
// completed tick scores changes both ratios and the balanced interaction term.
function scoreManualEqualEpoch(
  rounds,
  mode,
  {
    epochBudget = 100,
    serviceWeight = 1,
    totalServiceWeight = 1,
    rarityCoefficient = 0.25,
    rarityMinOpponents = 4,
    startRound = 1,
  } = {},
) {
  assert(Array.isArray(rounds) && rounds.length > 0, 'manual epoch requires at least one round')
  const teamCount = rounds[0].teamCount
  assert(
    Number.isInteger(teamCount) && teamCount >= 2,
    'manual epoch teamCount must be at least 2',
  )
  assert(Number.isFinite(epochBudget) && epochBudget > 0, 'epochBudget must be positive')
  assert(
    Number.isFinite(serviceWeight) && serviceWeight >= 0.8 && serviceWeight <= 1.2,
    'serviceWeight must be in [0.8, 1.2]',
  )
  assert(
    Number.isFinite(totalServiceWeight) && totalServiceWeight >= serviceWeight,
    'totalServiceWeight must be finite and at least serviceWeight',
  )
  assert(
    Number.isFinite(rarityCoefficient)
      && rarityCoefficient >= 0
      && rarityCoefficient <= 0.25,
    'rarityCoefficient must be in [0, 0.25]',
  )
  assert(
    Number.isInteger(rarityMinOpponents) && rarityMinOpponents >= 1,
    'rarityMinOpponents must be a positive integer',
  )
  assert(
    Number.isInteger(startRound) && startRound >= 1 && startRound <= rounds.length,
    'startRound must select a round in the supplied epoch',
  )
  assert(mode === 'arithmetic' || mode === 'balanced', 'unknown manual scoring mode')

  for (const round of rounds) {
    assert.equal(round.teamCount, teamCount, 'manual epoch requires a stable teamCount')
    assert(
      Array.isArray(round.captures) && round.captures.length === teamCount,
      'captures must match teamCount',
    )
    assert(
      Array.isArray(round.health) && round.health.length === teamCount,
      'health must match teamCount',
    )
    assert(
      Array.isArray(round.flagEligible) && round.flagEligible.length === teamCount,
      'flagEligible must match teamCount',
    )
    assert(round.flagEligible.every((value) => typeof value === 'boolean'), 'flagEligible must be boolean')
    for (const health of round.health) {
      assert(
        Number.isFinite(health) && health >= 0 && health <= 1,
        'health must be in [0, 1]',
      )
    }
    for (let victim = 0; victim < teamCount; victim += 1) {
      const captures = round.captures[victim]
      assert(captures instanceof Set, 'each captures entry must be a Set')
      for (const attacker of captures) {
        assert(
          Number.isInteger(attacker)
            && attacker >= 0
            && attacker < teamCount
            && attacker !== victim,
          'capturer must be an eligible opponent',
        )
      }
    }
  }

  const scores = blankScores(teamCount)
  const opponents = teamCount - 1
  const scoringRounds = rounds.slice(startRound - 1)
  const eligibleServiceTicks = scoringRounds.length
  const recipientBudget = epochBudget * serviceWeight / totalServiceWeight
  for (let team = 0; team < teamCount; team += 1) {
    let acceptedCaptures = 0
    let attackOpportunities = 0
    let rarityFractionSum = 0
    let defenseOpportunities = 0
    let protectedOpportunities = 0
    let eligibleAttackFlags = 0
    let eligibleDefenseFlags = 0
    let rawSlaCredit = 0

    for (const round of scoringRounds) {
      rawSlaCredit += round.health[team]
      for (let victim = 0; victim < teamCount; victim += 1) {
        if (victim === team) continue
        const offenseEligible = round.captures[victim].size > 0
          || (round.health[victim] > 0 && round.flagEligible[victim])
        if (!offenseEligible) continue
        eligibleAttackFlags += 1
        attackOpportunities += 1
        if (!round.captures[victim].has(team)) continue

        acceptedCaptures += 1
        const capturerCount = round.captures[victim].size
        if (opponents >= rarityMinOpponents) {
          rarityFractionSum += (opponents - capturerCount) / opponents
        }
      }

      // Only exact healthy custom-check flags contribute defense opportunities.
      // R still retains this tick's raw checker credit independently.
      if (round.health[team] <= 0 || !round.flagEligible[team]) continue
      eligibleDefenseFlags += 1
      defenseOpportunities += opponents
      protectedOpportunities += opponents - round.captures[team].size
    }

    const captureCoverage = attackOpportunities === 0
      ? 0
      : acceptedCaptures / attackOpportunities
    const rarityRate = attackOpportunities === 0
      ? 0
      : rarityFractionSum / attackOpportunities
    const attackRate = Math.min(1, captureCoverage + rarityCoefficient * rarityRate)
    const rarityPremium = attackRate - captureCoverage
    const slaMultiplier = rawSlaCredit / eligibleServiceTicks
    const defenseRate = defenseOpportunities === 0
      ? 0
      : protectedOpportunities / defenseOpportunities

    const attackFactor = mode === 'arithmetic' ? 0.5 : 0.4
    const defenseFactor = mode === 'arithmetic' ? 0.5 : 0.4
    const balanceRate = mode === 'balanced'
      ? 0.2 * Math.sqrt(attackRate * defenseRate)
      : 0
    const score = scores[team]
    score.attack = recipientBudget * slaMultiplier * attackFactor * attackRate
    score.defense = recipientBudget * slaMultiplier * defenseFactor * defenseRate
    score.balance = recipientBudget * slaMultiplier * balanceRate
    score.attackRate = attackRate
    score.defenseRate = defenseRate
    score.slaMultiplier = slaMultiplier
    score.rarityPremium = rarityPremium
    score.captureCoverage = captureCoverage
    score.rarityRate = rarityRate
    score.rarityFractionSum = rarityFractionSum
    score.acceptedCaptures = acceptedCaptures
    score.attackOpportunities = attackOpportunities
    score.defenseOpportunities = defenseOpportunities
    score.protectedOpportunities = protectedOpportunities
    score.eligibleAttackFlags = eligibleAttackFlags
    score.eligibleDefenseFlags = eligibleDefenseFlags
    score.rawSlaCredit = rawSlaCredit
    score.eligibleServiceTicks = eligibleServiceTicks
    score.recipientBudget = recipientBudget
    score.serviceWeight = serviceWeight
    score.startRound = startRound
    score.inputRoundCount = rounds.length
    score.scoredRoundCount = scoringRounds.length
  }
  return finish(scores)
}

export function scoreManualArithmetic(round, options = {}) {
  return scoreManualEqualEpoch([round], 'arithmetic', options)
}

export function scoreManualBalanced(round, options = {}) {
  return scoreManualEqualEpoch([round], 'balanced', options)
}

export function scoreManualArithmeticEpoch(rounds, options = {}) {
  return scoreManualEqualEpoch(rounds, 'arithmetic', options)
}

export function scoreManualBalancedEpoch(rounds, options = {}) {
  return scoreManualEqualEpoch(rounds, 'balanced', options)
}

// iCTF 2021's fixed team-service pot: a healthy unexploited owner keeps it,
// attackers split it after a compromise, and a down owner's pot is shared by
// healthy teams. There is no separate SLA component in this formula.
export function scoreIctf(round, { pool = 1 } = {}) {
  assert(Number.isFinite(pool) && pool > 0, 'pool must be finite and positive')
  const scores = blankScores(round.teamCount)
  const healthy = round.health
    .map((value, team) => ({ value, team }))
    .filter(({ value }) => value > 0)
    .map(({ team }) => team)

  for (let victim = 0; victim < round.teamCount; victim += 1) {
    const capturers = round.captures[victim]
    if (round.health[victim] <= 0) {
      if (healthy.length > 0) {
        const share = pool / healthy.length
        for (const team of healthy) scores[team].defense += share
      }
    } else if (capturers.size === 0) {
      scores[victim].defense += pool
    } else {
      const share = pool / capturers.size
      for (const attacker of capturers) scores[attacker].attack += share
    }
  }
  return finish(scores)
}

// No-rarity alternative inspired by ECSC's active-attacker idea. Only teams
  // that demonstrate a working exploit create pairwise pots. A valid capture
  // wins the pot regardless of checker state; without a capture, a healthy
  // target receives it and a down target burns it.
export function scoreActiveMatrix(round, { pool = 1 } = {}) {
  assert(Number.isFinite(pool) && pool > 0, 'pool must be finite and positive')
  const scores = blankScores(round.teamCount)
  const fieldScale = pool * Math.sqrt(round.teamCount)
  const pairPot = fieldScale / (round.teamCount - 1)
  const activeAttackers = new Set()

  for (const capturers of round.captures) {
    for (const attacker of capturers) activeAttackers.add(attacker)
  }
  addQuality(scores, round, pool)

  for (const attacker of activeAttackers) {
    for (let victim = 0; victim < round.teamCount; victim += 1) {
      if (victim === attacker) continue
      if (round.captures[victim].has(attacker)) {
        scores[attacker].attack += pairPot
      } else if (round.health[victim] > 0) {
        scores[victim].defense += pairPot * round.health[victim]
      }
    }
  }
  return finish(scores)
}

// Bounded non-conserved comparison control. Offense and compromise both use
// field-normalized coverage, so one capture's gain/loss ratio is independent of
// field size. Capturer count never changes an existing attacker's points.
export function scoreBounded(
  round,
  { attackWeight = 0.4, defenseWeight = 0.35, qualityWeight = 0.25 } = {},
) {
  for (const weight of [attackWeight, defenseWeight, qualityWeight]) {
    assert(Number.isFinite(weight) && weight >= 0, 'bounded weights must be finite and nonnegative')
  }
  const weightSum = attackWeight + defenseWeight + qualityWeight
  assert(Math.abs(weightSum - 1) < 1e-9, 'bounded weights must sum to 1')
  const scores = blankScores(round.teamCount)
  const opponents = round.teamCount - 1

  for (let team = 0; team < round.teamCount; team += 1) {
    let capturedTargets = 0
    for (let victim = 0; victim < round.teamCount; victim += 1) {
      if (victim === team || !round.captures[victim].has(team)) continue
      capturedTargets += 1
    }
    const offense = capturedTargets / opponents
    const compromiseCoverage = round.captures[team].size / opponents
    const quality = round.health[team]
    const defense = quality * (1 - compromiseCoverage)
    scores[team].attack = 100 * attackWeight * offense
    scores[team].defense = 100 * defenseWeight * defense
    scores[team].quality = 100 * qualityWeight * quality
  }
  return finish(scores)
}

export const SCORERS = Object.freeze({
  [FORMULAS.current]: scoreCurrent,
  [FORMULAS.sqrt]: scoreSqrtTransfer,
  [FORMULAS.flat]: scoreFlatTransfer,
  [FORMULAS.normalized]: scoreNormalizedCoverage,
  [FORMULAS.scarcity]: scoreBoundedScarcity,
  [FORMULAS.maturity]: scoreMaturityScarcity,
  [FORMULAS.positiveDefense]: scoreMaturityPositiveDefense,
  [FORMULAS.manualArithmetic]: scoreManualArithmetic,
  [FORMULAS.manualBalanced]: scoreManualBalanced,
  [FORMULAS.ictf]: scoreIctf,
  [FORMULAS.matrix]: scoreActiveMatrix,
  [FORMULAS.bounded]: scoreBounded,
})

export function mean(values) {
  return values.reduce((sum, value) => sum + value, 0) / values.length
}

export function aggregateEpochs(epochValues, mode = 'equal') {
  assert(epochValues.length > 0, 'at least one epoch is required')
  const all = mean(epochValues)
  if (mode === 'equal') return all
  const recent = mean(epochValues.slice(Math.floor(epochValues.length / 2)))
  if (mode === 'moderate-recency') return 0.7 * all + 0.3 * recent
  if (mode === 'heavy-recency') return 0.4 * all + 0.6 * recent
  throw new Error(`unknown epoch aggregation mode: ${mode}`)
}

export function summarizeEpochTotals(
  epochValues,
  {
    finalizedEpochCount = epochValues.length,
    gameEnded = false,
    configuredTicksPerEpoch = 1,
    epochTickCounts = null,
  } = {},
) {
  assert(Array.isArray(epochValues) && epochValues.length > 0, 'at least one epoch is required')
  assert(epochValues.every(Number.isFinite), 'epoch values must be finite')
  assert(
    Number.isInteger(configuredTicksPerEpoch) && configuredTicksPerEpoch >= 1,
    'configuredTicksPerEpoch must be a positive integer',
  )
  const tickCounts = epochTickCounts === null
    ? Array.from({ length: epochValues.length }, () => configuredTicksPerEpoch)
    : [...epochTickCounts]
  assert.equal(tickCounts.length, epochValues.length, 'epochTickCounts must match epoch values')
  assert(
    tickCounts.every((ticks) => (
      Number.isInteger(ticks) && ticks >= 1 && ticks <= configuredTicksPerEpoch
    )),
    'epoch tick counts must be within the configured epoch length',
  )
  assert(
    tickCounts.slice(0, -1).every((ticks) => ticks === configuredTicksPerEpoch),
    'only the final epoch may be partial',
  )
  assert(
    Number.isInteger(finalizedEpochCount)
      && finalizedEpochCount >= 0
      && finalizedEpochCount <= epochValues.length,
    'finalizedEpochCount must be within the epoch list',
  )
  assert(typeof gameEnded === 'boolean', 'gameEnded must be boolean')

  const effectiveFinalizedCount = gameEnded ? epochValues.length : finalizedEpochCount
  const epochWeights = tickCounts.map((ticks) => ticks / configuredTicksPerEpoch)
  const weightedMean = (values, weights) => {
    const denominator = weights.reduce((sum, weight) => sum + weight, 0)
    return values.reduce((sum, value, index) => sum + value * weights[index], 0) / denominator
  }
  return {
    settledTotal: effectiveFinalizedCount === 0
      ? 0
      : weightedMean(
        epochValues.slice(0, effectiveFinalizedCount),
        epochWeights.slice(0, effectiveFinalizedCount),
      ),
    projectedTotal: weightedMean(epochValues, epochWeights),
    finalizedEpochCount: effectiveFinalizedCount,
    epochCount: epochValues.length,
    configuredTicksPerEpoch,
    epochTickCounts: tickCounts,
    epochWeights,
  }
}

export function mulberry32(seed) {
  let state = seed >>> 0
  return () => {
    state += 0x6d2b79f5
    let value = state
    value = Math.imul(value ^ (value >>> 15), value | 1)
    value ^= value + Math.imul(value ^ (value >>> 7), value | 61)
    return ((value ^ (value >>> 14)) >>> 0) / 4294967296
  }
}

export function assertConserved(scores, epsilon = 1e-9) {
  const attack = scores.reduce((sum, score) => sum + score.attack, 0)
  const defense = -scores.reduce((sum, score) => sum + Math.min(0, score.defense), 0)
  assert(Math.abs(attack - defense) <= epsilon, `not conserved: attack=${attack}, loss=${defense}`)
}
