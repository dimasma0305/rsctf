import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import {
  CHECK_STATUS,
  FORMULAS,
  SCORERS,
  addCapture,
  aggregateEpochs,
  exclusiveSweep,
  makeRound,
  mean,
  mulberry32,
  scoreBounded,
  scoreMaturityPositiveDefense,
  scoreMaturityScarcity,
  scoreManualArithmeticEpoch,
  scoreManualBalancedEpoch,
  singleFlag,
  summarizeEpochTotals,
  tickCredit,
} from './lib.mjs'

const SEED = 0x5c0a1a6
const SAMPLE_SEED = (SEED ^ 0x20) >>> 0
const TRIALS = 1000
const TEAM_COUNT = 20
const EPOCHS = 6
const TICKS_PER_EPOCH = 8
const TOTAL_TICKS = EPOCHS * TICKS_PER_EPOCH
const HISTORICAL_FIELD_SIZES = [5, 8, 60, 250, 300]
const FIELD_SIZES = [5, 8, 20, 60, 250, 300]
const MODES = ['equal', 'moderate-recency', 'heavy-recency']
const OFFENSE_HEAVY = 'bounded-coverage-normalized-45-30-25'
const PRIMARY_FORMULAS = Object.freeze([
  FORMULAS.manualBalanced,
  FORMULAS.manualArithmetic,
  FORMULAS.positiveDefense,
])
const MANUAL_FORMULAS = new Set([
  FORMULAS.manualBalanced,
  FORMULAS.manualArithmetic,
])

const EVALUATED_SCORERS = Object.freeze({
  ...SCORERS,
  [OFFENSE_HEAVY]: (round) => scoreBounded(round, {
    attackWeight: 0.45,
    defenseWeight: 0.30,
    qualityWeight: 0.25,
  }),
})

function roundNumber(value, digits = 4) {
  return Number(value.toFixed(digits))
}

function ratioOrNull(numerator, denominator, digits = 3) {
  return Math.abs(denominator) <= 1e-12 ? null : roundNumber(numerator / denominator, digits)
}

function defenseDamage(clean, attacked, victim = 0) {
  return Math.max(0, clean[victim].defense - attacked[victim].defense)
}

function activeElsewhereControl(teamCount, activeCount) {
  const round = makeRound(teamCount)
  for (let attacker = 1; attacker <= activeCount; attacker += 1) {
    const victim = attacker === teamCount - 1 ? 1 : attacker + 1
    addCapture(round, attacker, victim)
  }
  return round
}

function maturityWeight(activeAttackers) {
  const progress = clamp((activeAttackers - 4) / 4)
  return progress * progress * (3 - 2 * progress)
}

function fullyMatureContext(teamCount) {
  return Array.from({ length: teamCount }, (_, victim) =>
    new Set(Array.from({ length: teamCount }, (_, team) => team).filter((team) => team !== victim)),
  )
}

function usesMaturityContext(formula) {
  return formula === FORMULAS.maturity || formula === FORMULAS.positiveDefense
}

function isManualFormula(formula) {
  return MANUAL_FORMULAS.has(formula)
}

// Qualification is victim-relative: evidence against the flag's owner does not
// help qualify an attacker for that same owner. Two targets in one mint tick or
// one target across two ticks is also insufficient; both dimensions must reach 2.
function buildQualifiedAttackersByVictim(rounds) {
  if (rounds.length === 0) throw new Error('maturity context requires at least one round')
  const teamCount = rounds[0].teamCount
  if (rounds.some((round) => round.teamCount !== teamCount)) {
    throw new Error('maturity context requires a stable epoch field')
  }

  return Array.from({ length: teamCount }, (_, excludedVictim) => {
    const evidence = Array.from({ length: teamCount }, () => ({
      victims: new Set(),
      roundIndices: new Set(),
    }))
    for (let roundIndex = 0; roundIndex < rounds.length; roundIndex += 1) {
      const round = rounds[roundIndex]
      for (let victim = 0; victim < teamCount; victim += 1) {
        if (victim === excludedVictim) continue
        for (const attacker of round.captures[victim]) {
          if (attacker === excludedVictim) continue
          evidence[attacker].victims.add(victim)
          evidence[attacker].roundIndices.add(roundIndex)
        }
      }
    }
    return new Set(
      evidence
        .map((value, attacker) => ({ ...value, attacker }))
        .filter(({ attacker, victims, roundIndices }) =>
          attacker !== excludedVictim && victims.size >= 2 && roundIndices.size >= 2,
        )
        .map(({ attacker }) => attacker),
    )
  })
}

function scoreFormulaRound(formula, round, qualifiedAttackersByVictim = null) {
  if (formula === FORMULAS.maturity) {
    return scoreMaturityScarcity(round, { qualifiedAttackersByVictim })
  }
  if (formula === FORMULAS.positiveDefense) {
    return scoreMaturityPositiveDefense(round, { qualifiedAttackersByVictim })
  }
  const scorer = EVALUATED_SCORERS[formula]
  if (scorer === undefined) throw new Error(`unknown scoring formula: ${formula}`)
  return scorer(round)
}

function scoreFormulaEpoch(formula, rounds) {
  if (rounds.length === 0) throw new Error('scoring an epoch requires at least one round')
  if (formula === FORMULAS.manualArithmetic) return scoreManualArithmeticEpoch(rounds)
  if (formula === FORMULAS.manualBalanced) return scoreManualBalancedEpoch(rounds)

  const totals = Array.from({ length: rounds[0].teamCount }, () => 0)
  const context = usesMaturityContext(formula)
    ? buildQualifiedAttackersByVictim(rounds)
    : null
  for (const round of rounds) {
    const scores = scoreFormulaRound(formula, round, context)
    for (let team = 0; team < totals.length; team += 1) totals[team] += scores[team].total
  }
  return totals.map((total, team) => ({ team, total: total / rounds.length }))
}

function formulaMetrics(formula, teamCount = 20) {
  const matureContext = fullyMatureContext(teamCount)
  const evaluate = (round) => scoreFormulaRound(
    formula,
    round,
    usesMaturityContext(formula) ? matureContext : null,
  )
  const opponents = teamCount - 1
  const unique = evaluate(singleFlag(teamCount, 1))
  const copiedOnce = evaluate(singleFlag(teamCount, 2))
  const copiedByField = evaluate(singleFlag(teamCount, opponents))
  const uniqueControl = evaluate(activeElsewhereControl(teamCount, 1))
  const copiedControl = evaluate(activeElsewhereControl(teamCount, opponents))
  const sweepUpRound = exclusiveSweep(teamCount)
  const sweepDownRound = exclusiveSweep(teamCount)
  sweepDownRound.health[0] = 0
  const sweepUp = evaluate(sweepUpRound)
  const sweepDown = evaluate(sweepDownRound)
  const pioneerAttack = unique[1].attack
  const copiedOnceAttack = copiedOnce[1].attack
  const copiedByFieldAttack = copiedByField[1].attack
  const uniqueCoalitionAttack = unique.reduce((sum, score) => sum + score.attack, 0)
  const copiedCoalitionAttack = copiedByField.reduce((sum, score) => sum + score.attack, 0)
  const uniqueDamage = defenseDamage(uniqueControl, unique)
  const copiedDamage = defenseDamage(copiedControl, copiedByField)

  // Team 1 succeeds elsewhere while team 0 remains uncaptured. Matrix-style
  // scoring treats that observable non-capture as defense, not proof of a block.
  const uncapturedRound = makeRound(teamCount)
  for (let victim = 2; victim < teamCount; victim += 1) addCapture(uncapturedRound, 1, victim)
  const uncapturedScores = evaluate(uncapturedRound)
  const isPositiveDefense = formula === FORMULAS.positiveDefense
  const positiveDefenseIssuance = isPositiveDefense
    ? uncapturedScores.reduce((sum, score) => sum + Math.max(0, score.defense), 0)
    : null
  const competitiveIssuance = isPositiveDefense
    ? uncapturedScores.reduce(
      (sum, score) => sum + score.attack + Math.max(0, score.defense),
      0,
    )
    : null
  const oldMaturityAttackIssuance = isPositiveDefense
    ? scoreMaturityScarcity(uncapturedRound, {
      qualifiedAttackersByVictim: matureContext,
    }).reduce((sum, score) => sum + score.attack, 0)
    : null

  return {
    formula,
    secondCopyRetentionPct: ratioOrNull(copiedOnceAttack * 100, pioneerAttack, 2),
    massCopyRetentionPct: ratioOrNull(copiedByFieldAttack * 100, pioneerAttack, 2),
    exclusiveToCommodityRatio: ratioOrNull(pioneerAttack, copiedByFieldAttack),
    coalitionGainMultiple: ratioOrNull(copiedCoalitionAttack, uniqueCoalitionAttack),
    victimDamageAmplification: isPositiveDefense
      ? null
      : ratioOrNull(copiedDamage, uniqueDamage),
    marginalVictimDamageToAttack: isPositiveDefense || uniqueDamage <= 1e-12
      ? null
      : ratioOrNull(uniqueDamage, pioneerAttack),
    exclusiveSweepAttack: roundNumber(sweepUp[0].attack),
    exclusiveSweepQuality: roundNumber(sweepUp[0].quality),
    downtimePenaltySameAttack: roundNumber(sweepUp[0].total - sweepDown[0].total),
    uncapturedTargetDefense: roundNumber(uncapturedScores[0].defense),
    positiveDefendedTargetAdvantage: isPositiveDefense
      ? roundNumber(uncapturedScores[0].defense)
      : null,
    positiveDefenseIssuance: isPositiveDefense
      ? roundNumber(positiveDefenseIssuance)
      : null,
    competitiveIssuance: isPositiveDefense ? roundNumber(competitiveIssuance) : null,
    competitiveIssuancePctOfOld: isPositiveDefense
      ? ratioOrNull(100 * competitiveIssuance, oldMaturityAttackIssuance, 2)
      : null,
  }
}

function fieldScaling() {
  const selected = [
    FORMULAS.current,
    FORMULAS.sqrt,
    FORMULAS.flat,
    FORMULAS.normalized,
    FORMULAS.scarcity,
    FORMULAS.maturity,
    FORMULAS.positiveDefense,
    FORMULAS.matrix,
    FORMULAS.bounded,
  ]
  return FIELD_SIZES.map((teamCount) => ({
    teamCount,
    formulas: Object.fromEntries(
      selected.map((formula) => {
        const context = usesMaturityContext(formula) ? fullyMatureContext(teamCount) : null
        const score = scoreFormulaRound(formula, exclusiveSweep(teamCount), context)[0]
        return [
          formula,
          {
            attack: roundNumber(score.attack),
            quality: roundNumber(score.quality),
            attackToQuality: ratioOrNull(score.attack, score.quality, 4),
          },
        ]
      }),
    ),
  }))
}

function scoreEpochCampaign(epochRounds, formula, mode) {
  const epochScores = epochRounds.map((rounds) =>
    scoreFormulaEpoch(formula, rounds).map((score) => score.total),
  )
  return Array.from({ length: epochScores[0].length }, (_, team) =>
    aggregateEpochs(epochScores.map((scores) => scores[team]), mode),
  )
}

function deterministicCampaigns(teamCount = 20) {
  const earlyVsLate = Array.from({ length: EPOCHS }, (_, epoch) => {
    if (epoch < 2) return [exclusiveSweep(teamCount, 0)]
    if (epoch >= EPOCHS - 2) return [exclusiveSweep(teamCount, 1)]
    return [makeRound(teamCount)]
  })

  const statusHistory = Array.from({ length: teamCount }, () => null)
  const defenseCampaign = []
  const recoveringStatuses = [
    CHECK_STATUS.offline,
    CHECK_STATUS.ok,
    CHECK_STATUS.ok,
    CHECK_STATUS.ok,
  ]
  for (let tick = 0; tick < recoveringStatuses.length; tick += 1) {
    const statuses = Array.from({ length: teamCount }, () => CHECK_STATUS.ok)
    statuses[1] = recoveringStatuses[tick]
    const credits = statuses.map((status, team) => {
      const credit = tickCredit(status, statusHistory[team])
      statusHistory[team] = status
      return credit
    })
    const round = makeRound(teamCount, credits)
    if (tick > 0) {
      for (let victim = 2; victim < teamCount; victim += 1) addCapture(round, 1, victim)
    }
    defenseCampaign.push(round)
  }

  return Object.keys(EVALUATED_SCORERS).map((formula) => {
    const equal = scoreEpochCampaign(earlyVsLate, formula, 'equal')
    const moderate = scoreEpochCampaign(earlyVsLate, formula, 'moderate-recency')
    const heavy = scoreEpochCampaign(earlyVsLate, formula, 'heavy-recency')
    let defenseTotals
    if (isManualFormula(formula)) {
      defenseTotals = scoreFormulaEpoch(formula, defenseCampaign).map((score) => score.total)
    } else {
      defenseTotals = Array.from({ length: teamCount }, () => 0)
      const defenseContext = usesMaturityContext(formula)
        ? buildQualifiedAttackersByVictim(defenseCampaign)
        : null
      for (const round of defenseCampaign) {
        const scores = scoreFormulaRound(formula, round, defenseContext)
        for (let team = 0; team < teamCount; team += 1) {
          defenseTotals[team] += scores[team].total
        }
      }
    }
    return {
      formula,
      earlyVsLateEqualDelta: roundNumber(equal[1] - equal[0]),
      earlyVsLateModerateDelta: roundNumber(moderate[1] - moderate[0]),
      earlyVsLateHeavyDelta: roundNumber(heavy[1] - heavy[0]),
      uncapturedVsBreachedAdvantage: roundNumber(defenseTotals[0] - defenseTotals[2]),
      recoveringAttackerVsHealthyDelta: roundNumber(defenseTotals[1] - defenseTotals[0]),
    }
  })
}

function clamp(value, min = 0, max = 1) {
  return Math.max(min, Math.min(max, value))
}

function interpolate(start, end, epoch) {
  return start + ((end - start) * epoch) / (EPOCHS - 1)
}

function buildProfiles(teamCount, rng) {
  const profiles = [
    {
      name: 'fast-agent',
      attack: (epoch) => interpolate(0.96, 0.50, epoch),
      defense: (epoch) => interpolate(0.48, 0.78, epoch),
      availability: 0.92,
      aiAdoption: 0.96,
      execution: 0.99,
    },
    {
      name: 'adaptive-human-ai',
      attack: (epoch) => interpolate(0.10, 0.96, epoch),
      defense: (epoch) => interpolate(0.70, 0.96, epoch),
      availability: 0.985,
      aiAdoption: 0.84,
      execution: 0.96,
    },
    {
      name: 'defense-specialist',
      attack: (epoch) => interpolate(0.03, 0.40, epoch),
      defense: () => 0.97,
      availability: 0.992,
      aiAdoption: 0.58,
      execution: 0.91,
    },
  ]

  while (profiles.length < teamCount) {
    const teamNumber = profiles.length + 1
    const attackStart = 0.18 + rng() * 0.45
    const attackEnd = clamp(attackStart + (rng() - 0.30) * 0.38)
    const defenseStart = 0.54 + rng() * 0.30
    const defenseEnd = clamp(defenseStart + rng() * 0.20)
    profiles.push({
      name: `field-${String(teamNumber).padStart(2, '0')}`,
      attack: (epoch) => interpolate(attackStart, attackEnd, epoch),
      defense: (epoch) => interpolate(defenseStart, defenseEnd, epoch),
      availability: 0.91 + rng() * 0.08,
      aiAdoption: 0.50 + rng() * 0.47,
      execution: 0.86 + rng() * 0.13,
    })
  }
  return profiles
}

function sampleStatus(profile, rng) {
  if (rng() < profile.availability) return CHECK_STATUS.ok
  return rng() < 0.65 ? CHECK_STATUS.offline : CHECK_STATUS.mumble
}

function buildExploitPaths(teamCount, rng) {
  const baseReleases = [0, 9, 18, 28, 38]
  return baseReleases.map((baseRelease) => ({
    releaseTick: Math.min(TOTAL_TICKS - 1, baseRelease + Math.floor(rng() * 3)),
    discoverability: 0.65 + rng() * 0.35,
    copyability: 0.70 + rng() * 0.30,
    copyDelay: 1 + Math.floor(rng() * 4),
    firstDiscoveryTick: null,
    known: Array.from({ length: teamCount }, () => false),
    patched: Array.from({ length: teamCount }, () => false),
    firstExposedTick: Array.from({ length: teamCount }, () => null),
  }))
}

function campaignDiagnostics(epochs, paths) {
  let capturedFlags = 0
  let capturerTotal = 0
  let multiCapturerFlags = 0
  let massDiffusionFlags = 0
  let attackEligibleFlags = 0
  let defenseEligibleFlags = 0
  let attackOpportunities = 0
  let acceptedCaptureOpportunities = 0
  let defenseOpportunities = 0
  let protectedDefenseOpportunities = 0
  let rarityFractionTotal = 0
  for (const rounds of epochs) {
    for (const round of rounds) {
      const opponents = round.teamCount - 1
      for (let victim = 0; victim < round.teamCount; victim += 1) {
        const capturers = round.captures[victim]
        const defenseEligible = round.health[victim] > 0 && round.flagEligible[victim]
        const attackEligible = defenseEligible || capturers.size > 0
        if (attackEligible) {
          attackEligibleFlags += 1
          attackOpportunities += opponents
          acceptedCaptureOpportunities += capturers.size
          if (opponents >= 4) {
            rarityFractionTotal += capturers.size * (opponents - capturers.size) / opponents
          }
        }
        if (defenseEligible) {
          defenseEligibleFlags += 1
          defenseOpportunities += opponents
          protectedDefenseOpportunities += opponents - capturers.size
        }
        if (capturers.size === 0) continue
        capturedFlags += 1
        capturerTotal += capturers.size
        if (capturers.size >= 2) multiCapturerFlags += 1
        if (capturers.size >= Math.ceil((round.teamCount - 1) / 2)) massDiffusionFlags += 1
      }
    }
  }
  const discoveredPaths = paths.filter((path) => path.firstDiscoveryTick !== null)
  return {
    capturedFlags,
    capturerTotal,
    multiCapturerFlags,
    massDiffusionFlags,
    attackEligibleFlags,
    defenseEligibleFlags,
    attackOpportunities,
    acceptedCaptureOpportunities,
    defenseOpportunities,
    protectedDefenseOpportunities,
    rarityFractionTotal,
    exploitPaths: paths.length,
    discoveredPaths: discoveredPaths.length,
    discoveryDelayTotal: discoveredPaths.reduce(
      (sum, path) => sum + path.firstDiscoveryTick - path.releaseTick,
      0,
    ),
  }
}

// Captures are correlated through shared exploit paths. A path must first be
// discovered, can then diffuse/rediscover after a delay, and is patched by each
// target independently after that target has been exposed.
function generateCampaign(rng) {
  const profiles = buildProfiles(TEAM_COUNT, rng)
  const paths = buildExploitPaths(TEAM_COUNT, rng)
  const previousStatuses = Array.from({ length: TEAM_COUNT }, () => null)
  const epochs = Array.from({ length: EPOCHS }, () => [])

  for (let globalTick = 0; globalTick < TOTAL_TICKS; globalTick += 1) {
    const epoch = Math.floor(globalTick / TICKS_PER_EPOCH)
    const statuses = profiles.map((profile) => sampleStatus(profile, rng))
    const credits = statuses.map((status, team) => {
      const credit = tickCredit(status, previousStatuses[team])
      previousStatuses[team] = status
      return credit
    })
    const round = makeRound(TEAM_COUNT, credits)

    for (const exploit of paths) {
      if (globalTick < exploit.releaseTick) continue

      for (let victim = 0; victim < TEAM_COUNT; victim += 1) {
        const exposedAt = exploit.firstExposedTick[victim]
        if (exploit.patched[victim] || exposedAt === null || exposedAt >= globalTick) continue
        const patchProbability = 0.025 + 0.30 * profiles[victim].defense(epoch) ** 1.35
        if (rng() < patchProbability) exploit.patched[victim] = true
      }

      for (let attacker = 0; attacker < TEAM_COUNT; attacker += 1) {
        if (exploit.known[attacker]) continue
        const attackSkill = profiles[attacker].attack(epoch)
        const discoveryProbability = 0.003 + 0.11 * exploit.discoverability * attackSkill ** 2
        let learned = rng() < discoveryProbability

        if (!learned && exploit.firstDiscoveryTick !== null) {
          const age = globalTick - exploit.firstDiscoveryTick
          if (age >= exploit.copyDelay) {
            const pressure = Math.min(1, (age - exploit.copyDelay + 1) / 7)
            const diffusionProbability =
              0.015 + 0.16 * exploit.copyability * profiles[attacker].aiAdoption * pressure
            learned = rng() < diffusionProbability
          }
        }

        if (learned) {
          exploit.known[attacker] = true
          if (exploit.firstDiscoveryTick === null) exploit.firstDiscoveryTick = globalTick
        }
      }

      for (let attacker = 0; attacker < TEAM_COUNT; attacker += 1) {
        if (!exploit.known[attacker]) continue
        const reliability = 0.68 + 0.30 * profiles[attacker].execution
        for (let victim = 0; victim < TEAM_COUNT; victim += 1) {
          if (attacker === victim) continue
          if (
            exploit.patched[victim]
            || statuses[victim] !== CHECK_STATUS.ok
          ) {
            continue
          }
          if (rng() < reliability) {
            addCapture(round, attacker, victim)
            if (exploit.firstExposedTick[victim] === null) {
              exploit.firstExposedTick[victim] = globalTick
            }
          }
        }
      }
    }
    epochs[epoch].push(round)
  }

  return { epochs, profiles, diagnostics: campaignDiagnostics(epochs, paths) }
}

function fractionalRank(values, team) {
  const value = values[team]
  let better = 0
  let equal = 0
  for (let other = 0; other < values.length; other += 1) {
    if (values[other] > value + 1e-9) better += 1
    else if (Math.abs(values[other] - value) <= 1e-9) equal += 1
  }
  return 1 + better + (equal - 1) / 2
}

function rankVector(values) {
  return values.map((__, team) => fractionalRank(values, team))
}

function topTeams(values, count) {
  return values
    .map((score, team) => ({ score, team }))
    .sort((left, right) => right.score - left.score || left.team - right.team)
    .slice(0, count)
    .map(({ team }) => team)
}

function pearsonCorrelation(left, right) {
  const leftMean = mean(left)
  const rightMean = mean(right)
  let numerator = 0
  let leftSquared = 0
  let rightSquared = 0
  for (let index = 0; index < left.length; index += 1) {
    const leftDelta = left[index] - leftMean
    const rightDelta = right[index] - rightMean
    numerator += leftDelta * rightDelta
    leftSquared += leftDelta ** 2
    rightSquared += rightDelta ** 2
  }
  const denominator = Math.sqrt(leftSquared * rightSquared)
  return denominator <= 1e-12 ? 1 : numerator / denominator
}

function compareRanks(referenceScores, comparisonScores) {
  const referenceRanks = rankVector(referenceScores)
  const comparisonRanks = rankVector(comparisonScores)
  const referenceTop = topTeams(referenceScores, 3)
  const comparisonTop = new Set(topTeams(comparisonScores, 3))
  return {
    meanAbsoluteRankShift: mean(
      referenceRanks.map((rank, team) => Math.abs(rank - comparisonRanks[team])),
    ),
    winnerFlip: topTeams(referenceScores, 1)[0] === topTeams(comparisonScores, 1)[0] ? 0 : 1,
    spearmanRho: pearsonCorrelation(referenceRanks, comparisonRanks),
    top3Overlap: referenceTop.filter((team) => comparisonTop.has(team)).length / 3,
  }
}

function scoreCampaignEpochs(campaign, formula) {
  return campaign.epochs.map((rounds) =>
    scoreFormulaEpoch(formula, rounds).map((score) => score.total),
  )
}

function maturityControls(teamCount = TEAM_COUNT) {
  const opponents = teamCount - 1
  const normalizedShare = 0.45 / opponents
  return [1, 4, 5, 6, 7, 8, opponents]
    .filter((activeAttackers, index, values) =>
      activeAttackers <= opponents && values.indexOf(activeAttackers) === index,
    )
    .map((activeAttackers) => {
      const context = Array.from({ length: teamCount }, () => new Set())
      context[0] = new Set(
        Array.from({ length: activeAttackers }, (_, offset) => offset + 1),
      )
      const scores = scoreMaturityScarcity(singleFlag(teamCount, 1), {
        qualifiedAttackersByVictim: context,
      })
      return {
        activeAttackers,
        maturityWeight: roundNumber(maturityWeight(activeAttackers), 4),
        soleCaptureAttack: roundNumber(scores[1].attack, 6),
        normalizedMultiplier: roundNumber(scores[1].attack / normalizedShare, 4),
        victimDebit: roundNumber(scores[0].defense, 6),
      }
    })
}

function collusionFunnelControl(teamCount = TEAM_COUNT) {
  const ally = 0
  const round = makeRound(teamCount)
  for (let attacker = 1; attacker < teamCount; attacker += 1) {
    for (let victim = 1; victim < teamCount; victim += 1) {
      if (victim !== attacker) addCapture(round, attacker, victim)
    }
  }
  const matureContext = fullyMatureContext(teamCount)
  const observational = scoreMaturityPositiveDefense(round, {
    qualifiedAttackersByVictim: matureContext,
  })[ally]
  const arithmetic = EVALUATED_SCORERS[FORMULAS.manualArithmetic](round)[ally]
  const balanced = EVALUATED_SCORERS[FORMULAS.manualBalanced](round)[ally]
  return {
    teamCount,
    directedMisses: teamCount - 1,
    observationalDefense: roundNumber(observational.defense, 6),
    observationalTotal: roundNumber(observational.total, 6),
    manualArithmeticDefense: roundNumber(arithmetic.defense, 6),
    manualArithmeticTotal: roundNumber(arithmetic.total, 6),
    manualBalancedDefense: roundNumber(balanced.defense, 6),
    manualBalancedTotal: roundNumber(balanced.total, 6),
  }
}

function campaignMaturityDiagnostics(campaign) {
  return campaign.epochs.map((rounds) => {
    const context = buildQualifiedAttackersByVictim(rounds)
    const totals = {
      victimEpochContexts: context.length,
      qualifiedAttackers: context.reduce((sum, attackers) => sum + attackers.size, 0),
      capturedFlags: 0,
      activeAttackers: 0,
      maturityWeight: 0,
      defaultWeightFlags: 0,
      fullWeightFlags: 0,
    }
    for (const round of rounds) {
      for (let victim = 0; victim < round.teamCount; victim += 1) {
        const capturers = round.captures[victim]
        if (capturers.size === 0) continue
        const activeAttackers = new Set([...context[victim], ...capturers]).size
        const weight = maturityWeight(activeAttackers)
        totals.capturedFlags += 1
        totals.activeAttackers += activeAttackers
        totals.maturityWeight += weight
        if (weight <= 1e-12) totals.defaultWeightFlags += 1
        if (weight >= 1 - 1e-12) totals.fullWeightFlags += 1
      }
    }
    return totals
  })
}

function scoreSampleFormula(campaign, formula) {
  const epochScores = scoreCampaignEpochs(campaign, formula)
  const totals = Array.from({ length: TEAM_COUNT }, (_, team) =>
    summarizeEpochTotals(epochScores.map((scores) => scores[team]), { gameEnded: true }),
  )
  const settledScores = totals.map(({ settledTotal }) => settledTotal)
  const teams = campaign.profiles.map((profile, team) => {
    const epochValues = epochScores.map((scores) => roundNumber(scores[team]))
    return {
      team: team + 1,
      profile: profile.name,
      epochOneRank: fractionalRank(epochScores[0], team),
      finalRank: fractionalRank(settledScores, team),
      epochRanks: epochScores.map((scores) => roundNumber(fractionalRank(scores, team), 2)),
      epochScores: epochValues,
      settledTotal: roundNumber(totals[team].settledTotal),
      projectedTotal: roundNumber(totals[team].projectedTotal),
    }
  })
  teams.sort((left, right) => left.finalRank - right.finalRank || left.team - right.team)
  const invalidTeam = teams.some((team, index) => {
    const roundedMean = mean(team.epochScores)
    return team.epochScores.length !== EPOCHS
      || team.epochScores.some((score) => !Number.isFinite(score))
      || !Number.isFinite(team.epochOneRank)
      || !Number.isFinite(team.finalRank)
      || !Number.isFinite(team.settledTotal)
      || !Number.isFinite(team.projectedTotal)
      || team.epochOneRank < 1
      || team.epochOneRank > TEAM_COUNT
      || team.finalRank < 1
      || team.finalRank > TEAM_COUNT
      || Math.abs(roundedMean - team.settledTotal) > 0.00011
      || Math.abs(roundedMean - team.projectedTotal) > 0.00011
      || (index > 0 && teams[index - 1].finalRank > team.finalRank)
  })
  if (
    teams.length !== TEAM_COUNT
    || new Set(teams.map((team) => team.team)).size !== TEAM_COUNT
    || invalidTeam
  ) {
    throw new Error('sample leaderboard shape does not match simulator metadata')
  }
  return {
    formula,
    epochMode: 'equal',
    teams,
  }
}

function runSampleComparisons() {
  const campaign = generateCampaign(mulberry32(SAMPLE_SEED))
  const scored = Object.fromEntries(
    PRIMARY_FORMULAS.map((formula) => [formula, scoreSampleFormula(campaign, formula)]),
  )
  return {
    sampleStandings: { seed: SAMPLE_SEED, ...scored[FORMULAS.manualBalanced] },
    sampleComparisons: {
      seed: SAMPLE_SEED,
      formulas: Object.fromEntries(
        Object.entries(scored).map(([formula, result]) => [
          formula,
          {
            teams: result.teams.map(({ team, profile, epochRanks, finalRank }) => ({
              team,
              profile,
              epochRanks,
              finalRank,
            })),
          },
        ]),
      ),
    },
  }
}

function runLastToFirstControl() {
  const comebackTeam = TEAM_COUNT - 1
  const firstRound = makeRound(TEAM_COUNT)
  for (let attacker = 0; attacker < comebackTeam; attacker += 1) {
    addCapture(firstRound, attacker, comebackTeam)
  }
  const epochs = [[firstRound]]
  for (let epoch = 1; epoch < EPOCHS; epoch += 1) {
    epochs.push([exclusiveSweep(TEAM_COUNT, comebackTeam)])
  }
  const campaign = {
    epochs,
    profiles: Array.from({ length: TEAM_COUNT }, (_, team) => ({ name: `team-${team + 1}` })),
  }
  const epochScores = scoreCampaignEpochs(campaign, FORMULAS.manualBalanced)
  const totals = Array.from({ length: TEAM_COUNT }, (_, team) =>
    summarizeEpochTotals(epochScores.map((scores) => scores[team]), { gameEnded: true }),
  )
  const settledScores = totals.map(({ settledTotal }) => settledTotal)
  const result = {
    team: comebackTeam + 1,
    epochOneRank: fractionalRank(epochScores[0], comebackTeam),
    finalRank: fractionalRank(settledScores, comebackTeam),
    epochScores: epochScores.map((scores) => roundNumber(scores[comebackTeam])),
    settledTotal: roundNumber(totals[comebackTeam].settledTotal),
    projectedTotal: roundNumber(totals[comebackTeam].projectedTotal),
    bestOpponentSettledTotal: roundNumber(Math.max(...settledScores.slice(0, comebackTeam))),
  }
  if (result.epochOneRank !== TEAM_COUNT || result.finalRank !== 1) {
    throw new Error(`last-to-first control failed: ${JSON.stringify(result)}`)
  }
  return result
}

function liveEpochTotalControl() {
  const epochValues = [80, 20]
  const configuredTicksPerEpoch = 8
  const epochTickCounts = [8, 1]
  const roundedTotals = (totals) => ({
    ...totals,
    settledTotal: roundNumber(totals.settledTotal),
    projectedTotal: roundNumber(totals.projectedTotal),
  })
  return {
    epochValues,
    configuredTicksPerEpoch,
    epochTickCounts,
    duringPlay: roundedTotals(summarizeEpochTotals(epochValues, {
      finalizedEpochCount: 1,
      configuredTicksPerEpoch,
      epochTickCounts,
    })),
    atGameEnd: roundedTotals(summarizeEpochTotals(epochValues, {
      finalizedEpochCount: 1,
      gameEnded: true,
      configuredTicksPerEpoch,
      epochTickCounts,
    })),
  }
}

function buildSlaScenario(scenario, uptime) {
  const health = Array.from({ length: TEAM_COUNT }, () => 1)
  health[0] = uptime
  if (scenario === 'exclusive-attacker') {
    return exclusiveSweep(TEAM_COUNT, 0, health)
  }
  if (scenario === 'uncaptured-defender') {
    const round = makeRound(TEAM_COUNT, health)
    for (let attacker = 1; attacker <= 8; attacker += 1) {
      addCapture(round, attacker, attacker + 1)
    }
    return round
  }
  throw new Error(`unknown SLA response scenario: ${scenario}`)
}

function buildSlaResponse() {
  const uptimes = [0, 0.25, 0.5, 0.75, 1]
  const scenarios = ['exclusive-attacker', 'uncaptured-defender']
  const matureContext = fullyMatureContext(TEAM_COUNT)
  const rows = []
  for (const formula of PRIMARY_FORMULAS) {
    for (const scenario of scenarios) {
      const scores = uptimes.map((uptime) => {
        const round = buildSlaScenario(scenario, uptime)
        const result = scoreFormulaRound(
          formula,
          round,
          usesMaturityContext(formula) ? matureContext : null,
        )[0]
        return { uptime, score: result.total }
      })
      const fullScore = scores.find(({ uptime }) => uptime === 1).score
      if (fullScore <= 1e-12) throw new Error(`empty SLA response baseline: ${formula}/${scenario}`)
      for (const { uptime, score } of scores) {
        rows.push({
          formula,
          scenario,
          uptime,
          nativeScore: roundNumber(score, 6),
          retentionPct: roundNumber(100 * score / fullScore, 2),
        })
      }
    }
  }
  return { rows }
}

function buildStakeholderExamples() {
  const inputs = [
    { label: 'Balanced team', attack: 0.6, defense: 0.6 },
    { label: 'Attack-heavy team', attack: 0.9, defense: 0.1 },
    { label: 'Attack only', attack: 1, defense: 0 },
  ]
  return inputs.map(({ label, attack, defense }) => {
    const teamCount = 11
    const opponents = teamCount - 1
    const round = makeRound(teamCount)
    for (let victim = 1; victim <= attack * opponents; victim += 1) {
      for (let attacker = 0; attacker < teamCount; attacker += 1) {
        if (attacker !== victim) addCapture(round, attacker, victim)
      }
    }
    const incomingCaptures = (1 - defense) * opponents
    for (let attacker = 1; attacker <= incomingCaptures; attacker += 1) {
      addCapture(round, attacker, 0)
    }
    const arithmetic = scoreFormulaRound(FORMULAS.manualArithmetic, round)[0]
    const balanced = scoreFormulaRound(FORMULAS.manualBalanced, round)[0]
    if (
      Math.abs(arithmetic.attackRate - attack) > 1e-12
      || Math.abs(arithmetic.defenseRate - defense) > 1e-12
    ) {
      throw new Error(`stakeholder example rate mismatch: ${label}`)
    }
    return {
      label,
      attack,
      defense,
      arithmeticScore: roundNumber(arithmetic.total, 2),
      balancedScore: roundNumber(balanced.total, 2),
    }
  })
}

function emptyMonteCarloResult() {
  return {
    trials: 0,
    winnerShare: { fastAgent: 0, adaptiveHumanAi: 0, defenseSpecialist: 0, field: 0 },
    averageRank: { fastAgent: 0, adaptiveHumanAi: 0, defenseSpecialist: 0 },
    earlyFastLeads: 0,
    adaptiveComebacks: 0,
  }
}

function runMonteCarlo() {
  const rng = mulberry32(SEED)
  const scores = Object.fromEntries(
    Object.keys(EVALUATED_SCORERS).map((formula) => [
      formula,
      Object.fromEntries(MODES.map((mode) => [mode, emptyMonteCarloResult()])),
    ]),
  )
  const diagnosticTotals = {
    capturedFlags: 0,
    capturerTotal: 0,
    multiCapturerFlags: 0,
    massDiffusionFlags: 0,
    attackEligibleFlags: 0,
    defenseEligibleFlags: 0,
    attackOpportunities: 0,
    acceptedCaptureOpportunities: 0,
    defenseOpportunities: 0,
    protectedDefenseOpportunities: 0,
    rarityFractionTotal: 0,
    exploitPaths: 0,
    discoveredPaths: 0,
    discoveryDelayTotal: 0,
  }
  const maturityTotals = Array.from({ length: EPOCHS }, () => ({
    victimEpochContexts: 0,
    qualifiedAttackers: 0,
    capturedFlags: 0,
    activeAttackers: 0,
    maturityWeight: 0,
    defaultWeightFlags: 0,
    fullWeightFlags: 0,
  }))
  const pairedTotals = Object.fromEntries(
    [FORMULAS.manualBalanced, FORMULAS.manualArithmetic].map((formula) => [
      formula,
      {
        trials: 0,
        meanAbsoluteRankShift: 0,
        winnerFlipRate: 0,
        meanSpearmanRho: 0,
        meanTop3Overlap: 0,
      },
    ]),
  )

  for (let trial = 0; trial < TRIALS; trial += 1) {
    const campaign = generateCampaign(rng)
    const equalScoresByFormula = {}
    for (const key of Object.keys(diagnosticTotals)) {
      diagnosticTotals[key] += campaign.diagnostics[key]
    }
    const maturityDiagnostics = campaignMaturityDiagnostics(campaign)
    for (let epoch = 0; epoch < EPOCHS; epoch += 1) {
      for (const key of Object.keys(maturityTotals[epoch])) {
        maturityTotals[epoch][key] += maturityDiagnostics[epoch][key]
      }
    }

    for (const formula of Object.keys(EVALUATED_SCORERS)) {
      const epochScores = scoreCampaignEpochs(campaign, formula)
      const earlyFastLead = mean(epochScores.slice(0, 2).map((values) => values[0]))
        > mean(epochScores.slice(0, 2).map((values) => values[1]))

      for (const mode of MODES) {
        const summary = scores[formula][mode]
        const finalScores = Array.from({ length: TEAM_COUNT }, (_, team) =>
          aggregateEpochs(epochScores.map((values) => values[team]), mode),
        )
        if (mode === 'equal') equalScoresByFormula[formula] = finalScores
        const maximum = Math.max(...finalScores)
        const winners = finalScores
          .map((score, team) => ({ score, team }))
          .filter(({ score }) => Math.abs(score - maximum) <= 1e-9)
          .map(({ team }) => team)
        const share = 1 / winners.length
        for (const winner of winners) {
          if (winner === 0) summary.winnerShare.fastAgent += share
          else if (winner === 1) summary.winnerShare.adaptiveHumanAi += share
          else if (winner === 2) summary.winnerShare.defenseSpecialist += share
          else summary.winnerShare.field += share
        }
        summary.averageRank.fastAgent += fractionalRank(finalScores, 0)
        summary.averageRank.adaptiveHumanAi += fractionalRank(finalScores, 1)
        summary.averageRank.defenseSpecialist += fractionalRank(finalScores, 2)
        summary.trials += 1
        if (earlyFastLead) {
          summary.earlyFastLeads += 1
          if (finalScores[1] > finalScores[0]) summary.adaptiveComebacks += 1
        }
      }
    }

    const referenceScores = equalScoresByFormula[FORMULAS.positiveDefense]
    for (const formula of Object.keys(pairedTotals)) {
      const comparison = compareRanks(referenceScores, equalScoresByFormula[formula])
      const totals = pairedTotals[formula]
      totals.trials += 1
      totals.meanAbsoluteRankShift += comparison.meanAbsoluteRankShift
      totals.winnerFlipRate += comparison.winnerFlip
      totals.meanSpearmanRho += comparison.spearmanRho
      totals.meanTop3Overlap += comparison.top3Overlap
    }
  }

  for (const modes of Object.values(scores)) {
    for (const summary of Object.values(modes)) {
      for (const key of Object.keys(summary.winnerShare)) {
        summary.winnerShare[key] = roundNumber(summary.winnerShare[key] / summary.trials, 2)
      }
      for (const key of Object.keys(summary.averageRank)) {
        summary.averageRank[key] = roundNumber(summary.averageRank[key] / summary.trials, 2)
      }
      const categorizedWinnerShare = Object.values(summary.winnerShare)
        .reduce((sum, value) => sum + value, 0)
      if (Math.abs(categorizedWinnerShare - 1) > 0.02) {
        throw new Error(`winner shares do not sum to one: ${categorizedWinnerShare}`)
      }
      summary.winnerShare.perFieldTeam = roundNumber(
        summary.winnerShare.field / (TEAM_COUNT - 3),
        3,
      )
      summary.adaptiveRankScore = roundNumber(
        100 * (TEAM_COUNT - summary.averageRank.adaptiveHumanAi) / (TEAM_COUNT - 1),
        1,
      )
      summary.adaptiveComebackRate = summary.earlyFastLeads === 0
        ? null
        : roundNumber(summary.adaptiveComebacks / summary.earlyFastLeads, 2)
    }
  }

  const diagnostics = {
    exploitPaths: diagnosticTotals.exploitPaths,
    discoveredPathFraction: ratioOrNull(
      diagnosticTotals.discoveredPaths,
      diagnosticTotals.exploitPaths,
      3,
    ),
    meanDiscoveryDelayTicks: ratioOrNull(
      diagnosticTotals.discoveryDelayTotal,
      diagnosticTotals.discoveredPaths,
      2,
    ),
    capturedFlags: diagnosticTotals.capturedFlags,
    meanCapturersPerCapturedFlag: ratioOrNull(
      diagnosticTotals.capturerTotal,
      diagnosticTotals.capturedFlags,
      2,
    ),
    multiCapturerFlagFraction: ratioOrNull(
      diagnosticTotals.multiCapturerFlags,
      diagnosticTotals.capturedFlags,
      3,
    ),
    massDiffusionFlagFraction: ratioOrNull(
      diagnosticTotals.massDiffusionFlags,
      diagnosticTotals.capturedFlags,
      3,
    ),
    attackEligibleFlags: diagnosticTotals.attackEligibleFlags,
    defenseEligibleFlags: diagnosticTotals.defenseEligibleFlags,
    attackOpportunities: diagnosticTotals.attackOpportunities,
    acceptedCaptureOpportunities: diagnosticTotals.acceptedCaptureOpportunities,
    attackCoverage: ratioOrNull(
      diagnosticTotals.acceptedCaptureOpportunities,
      diagnosticTotals.attackOpportunities,
      3,
    ),
    defenseOpportunities: diagnosticTotals.defenseOpportunities,
    protectedDefenseOpportunities: diagnosticTotals.protectedDefenseOpportunities,
    protectedDefenseFraction: ratioOrNull(
      diagnosticTotals.protectedDefenseOpportunities,
      diagnosticTotals.defenseOpportunities,
      3,
    ),
    meanRarityFractionPerAcceptedCapture: ratioOrNull(
      diagnosticTotals.rarityFractionTotal,
      diagnosticTotals.acceptedCaptureOpportunities,
      3,
    ),
    maturityByEpoch: maturityTotals.map((totals, epoch) => ({
      epoch: epoch + 1,
      meanQualifiedAttackersPerVictim: ratioOrNull(
        totals.qualifiedAttackers,
        totals.victimEpochContexts,
        2,
      ),
      meanActiveAttackersPerCapturedFlag: ratioOrNull(
        totals.activeAttackers,
        totals.capturedFlags,
        2,
      ),
      meanMaturityWeight: ratioOrNull(
        totals.maturityWeight,
        totals.capturedFlags,
        3,
      ),
      defaultWeightFlagFraction: ratioOrNull(
        totals.defaultWeightFlags,
        totals.capturedFlags,
        3,
      ),
      fullWeightFlagFraction: ratioOrNull(
        totals.fullWeightFlags,
        totals.capturedFlags,
        3,
      ),
    })),
  }
  if (
    diagnostics.multiCapturerFlagFraction < 0.25
    || diagnostics.massDiffusionFlagFraction < 0.02
  ) {
    throw new Error('AI diffusion guard failed: campaign no longer exercises common exploits')
  }
  const pairedComparisons = Object.fromEntries(
    Object.entries(pairedTotals).map(([formula, totals]) => [
      formula,
      {
        referenceFormula: FORMULAS.positiveDefense,
        trials: totals.trials,
        meanAbsoluteRankShift: roundNumber(totals.meanAbsoluteRankShift / totals.trials, 2),
        winnerFlipRate: roundNumber(totals.winnerFlipRate / totals.trials, 3),
        meanSpearmanRho: roundNumber(totals.meanSpearmanRho / totals.trials, 3),
        meanTop3Overlap: roundNumber(totals.meanTop3Overlap / totals.trials, 3),
      },
    ]),
  )
  return { scores, diagnostics, pairedComparisons }
}

function markdownTable(rows, columns) {
  const header = `| ${columns.map((column) => column.label).join(' | ')} |`
  const separator = `| ${columns.map(() => '---').join(' | ')} |`
  const body = rows.map((row) =>
    `| ${columns.map((column) => column.value(row) ?? 'n/a').join(' | ')} |`,
  )
  return [header, separator, ...body].join('\n')
}

function buildReport(results) {
  const metricColumns = [
    { label: 'Formula', value: (row) => row.formula },
    { label: 'k=2 retained', value: (row) => `${row.secondCopyRetentionPct}%` },
    { label: 'k=M retained', value: (row) => `${row.massCopyRetentionPct}%` },
    { label: 'Rare/common', value: (row) => row.exclusiveToCommodityRatio },
    { label: 'Coalition gain x', value: (row) => row.coalitionGainMultiple },
    { label: 'Victim damage x', value: (row) => row.victimDamageAmplification },
    { label: 'Victim/attack', value: (row) => row.marginalVictimDamageToAttack },
    { label: 'Defended +DEF', value: (row) => row.positiveDefendedTargetAdvantage },
    { label: 'ATK+DEF issued', value: (row) => row.competitiveIssuance },
    { label: '% old ATK', value: (row) => row.competitiveIssuancePctOfOld },
  ]
  const scenarioColumns = [
    { label: 'Formula', value: (row) => row.formula },
    { label: 'Equal late-early', value: (row) => row.earlyVsLateEqualDelta },
    { label: 'Moderate late-early', value: (row) => row.earlyVsLateModerateDelta },
    { label: 'Heavy late-early', value: (row) => row.earlyVsLateHeavyDelta },
    { label: 'Uncaptured advantage', value: (row) => row.uncapturedVsBreachedAdvantage },
  ]
  const monteRows = Object.entries(results.monteCarlo.scores).flatMap(([formula, modes]) =>
    Object.entries(modes).map(([mode, summary]) => ({ formula, mode, ...summary })),
  )
  const monteColumns = [
    { label: 'Formula', value: (row) => row.formula },
    { label: 'Epoch mode', value: (row) => row.mode },
    { label: 'Fast win', value: (row) => row.winnerShare.fastAgent },
    { label: 'Adaptive win', value: (row) => row.winnerShare.adaptiveHumanAi },
    { label: 'Defender win', value: (row) => row.winnerShare.defenseSpecialist },
    { label: 'Field win', value: (row) => row.winnerShare.field },
    { label: 'Field/team win', value: (row) => row.winnerShare.perFieldTeam },
    { label: 'Adaptive rank', value: (row) => row.averageRank.adaptiveHumanAi },
    { label: 'Adaptive rank score', value: (row) => row.adaptiveRankScore },
    { label: 'Comeback', value: (row) => row.adaptiveComebackRate },
  ]
  const scalingRows = results.fieldScaling.map((row) => ({
    teamCount: row.teamCount,
    current: row.formulas[FORMULAS.current].attackToQuality,
    sqrt: row.formulas[FORMULAS.sqrt].attackToQuality,
    flat: row.formulas[FORMULAS.flat].attackToQuality,
    normalized: row.formulas[FORMULAS.normalized].attackToQuality,
    scarcity: row.formulas[FORMULAS.scarcity].attackToQuality,
    maturity: row.formulas[FORMULAS.maturity].attackToQuality,
    positiveDefense: row.formulas[FORMULAS.positiveDefense].attackToQuality,
    matrix: row.formulas[FORMULAS.matrix].attackToQuality,
    bounded: row.formulas[FORMULAS.bounded].attackToQuality,
  }))
  const scalingColumns = [
    { label: 'Teams', value: (row) => row.teamCount },
    { label: 'Legacy 1/k', value: (row) => row.current },
    { label: 'Sqrt', value: (row) => row.sqrt },
    { label: 'Flat', value: (row) => row.flat },
    { label: 'Normalized', value: (row) => row.normalized },
    { label: 'Scarcity', value: (row) => row.scarcity },
    { label: 'Maturity -DEF', value: (row) => row.maturity },
    { label: 'Maturity +DEF', value: (row) => row.positiveDefense },
    { label: 'Matrix', value: (row) => row.matrix },
    { label: 'Bounded', value: (row) => row.bounded },
  ]
  const diagnostics = results.monteCarlo.diagnostics
  const positiveDefenseEqual = results.monteCarlo.scores[FORMULAS.positiveDefense].equal
  const manualBalancedEqual = results.monteCarlo.scores[FORMULAS.manualBalanced].equal
  const manualArithmeticEqual = results.monteCarlo.scores[FORMULAS.manualArithmetic].equal
  const slaColumns = [
    { label: 'Formula', value: (row) => row.formula },
    { label: 'Scenario', value: (row) => row.scenario },
    { label: 'U', value: (row) => row.uptime },
    { label: 'Retained', value: (row) => `${row.retentionPct}%` },
  ]
  const pairedRows = Object.entries(results.pairedComparisons).map(([formula, values]) => ({
    formula,
    ...values,
  }))
  const pairedColumns = [
    { label: 'Formula', value: (row) => row.formula },
    { label: 'Mean abs rank shift', value: (row) => row.meanAbsoluteRankShift },
    { label: 'Winner flip', value: (row) => row.winnerFlipRate },
    { label: 'Spearman rho', value: (row) => row.meanSpearmanRho },
    { label: 'Top-3 overlap', value: (row) => row.meanTop3Overlap },
  ]
  const sampleColumns = [
    { label: 'Final rank', value: (row) => row.finalRank },
    { label: 'Team', value: (row) => row.team },
    { label: 'Profile', value: (row) => row.profile },
    { label: 'E1 rank', value: (row) => row.epochOneRank },
    ...Array.from({ length: results.metadata.epochs }, (_, epoch) => ({
      label: `E${epoch + 1}`,
      value: (row) => row.epochScores[epoch],
    })),
    { label: 'Settled total', value: (row) => row.settledTotal },
    { label: 'Projected total', value: (row) => row.projectedTotal },
  ]
  const maturityControlColumns = [
    { label: 'A', value: (row) => row.activeAttackers },
    { label: 'w(A)', value: (row) => row.maturityWeight },
    { label: '-DEF sole ATK', value: (row) => row.soleCaptureAttack },
    { label: 'vs no-rarity', value: (row) => row.normalizedMultiplier },
    { label: '-DEF victim debit', value: (row) => row.victimDebit },
  ]
  const maturityDiagnosticColumns = [
    { label: 'Epoch', value: (row) => row.epoch },
    { label: 'Mean Q/victim', value: (row) => row.meanQualifiedAttackersPerVictim },
    { label: 'Mean A/captured flag', value: (row) => row.meanActiveAttackersPerCapturedFlag },
    { label: 'Mean w(A)', value: (row) => row.meanMaturityWeight },
    { label: 'w=0 fraction', value: (row) => row.defaultWeightFlagFraction },
    { label: 'w=1 fraction', value: (row) => row.fullWeightFlagFraction },
  ]

  return `# A&D Scoring Simulation Report

Generated deterministically by \`node tools/ad-scoring-sim/simulate.mjs\`.

## Decision

rsctf's deployed official A&D policy is \`EpochBalanced\`, represented by the simulator row \`manual-equal-balanced\`. It replaces the old \`1/k\` rarity pool for ranking and awards in AI-heavy events; \`manual-equal-arithmetic\` remains the arithmetic governance control. Both use only ordinary accepted flags, rotating-flag eligibility, and local checker SLA; teams keep and run their exploit tooling themselves.

Declare the global \`startRound\` only when at least two accepted teams have every enabled A&D service and every enabled A&D challenge has a prepared exact custom checker. Freeze the ranked team-service roster from the flags minted in that round and reuse it for every epoch. Teams or services absent from that snapshot, and captures attributed to them, do not enter official scoring. For each offense-eligible flag, let \`M=N-1\` be its frozen opponent count and \`k\` the distinct accepted frozen-roster capturers. An exact healthy custom check or any such accepted capture makes the flag offense-eligible, after which every frozen opponent receives the same attack opportunity. A capturer records one capture plus rarity fraction \`(M-k)/M\` when \`M>=4\`; otherwise the rarity fraction is zero.

Across an epoch, \`C=captures/attack opportunities\`, \`H=sum(rarity fractions)/attack opportunities\`, and \`A=min(1,C+0.25*H)\`. The rarity coefficient adds at most 25% of base capture coverage; after the \`A<=1\` clamp, the realized lift is never more than 20 percentage points. Accepted capturer count is its only input. A rare flag is a difficulty proxy, not proof of a patch bypass.

Only an exact healthy custom check creates \`M\` pairwise defense opportunities and \`M-k\` protected opportunities for the victim, so \`D=sum(M-k)/sum(M)\`. An accepted capture can preserve offense evidence during a checker failure, but it cannot mint defense eligibility. One rare bypass removes one pair instead of erasing the whole flag's defense. This is still observational: an unstolen pair does not prove that an exploit was attempted. A fallback TCP probe does not qualify. Let \`R\` be local checker SLA. The arithmetic control uses \`Core=0.5*A+0.5*D\`; the official balanced policy uses \`Core=0.4*A+0.4*D+0.2*sqrt(A*D)\`, and \`Local=100*R*Core\`. Evidence is aggregated over the epoch before applying the nonlinear core once.

Complete epochs each have weight 1. Production precommits \`n\` in \`[1,64]\` so the unresolved raw evidence window stays bounded. A live or final partial tail with \`r\` observed ticks out of \`n\` configured ticks has weight \`r/n\`, so a one-tick tail never receives a full epoch budget. During live play, \`settledTotal\` is the weighted average of finalized epochs only, while \`projectedTotal\` includes all current evidence and the fractional open tail. A normal epoch finalizes only after its last flag lifetime closes. Game end closes and finalizes the partial tail at the same fractional weight, after which settled and projected totals converge. The list UI retains the latest three epoch detail rows, but both totals use the complete epoch history.

Each service has a precommitted operator-set weight in \`[0.8,1.2]\`, snapshotted with the flag/round and normalized across services so the epoch ceiling remains 100. This is a modest adjustment for service sloppability or inherent difficulty, not dynamic rarity. The first complete ranked roster with prepared exact custom checkers establishes the published \`startRound\`; all earlier evidence is excluded.

## Evidence Boundary

At the 2026-07-10T22:45:34Z audit snapshot, the two surviving games contained zero attacks. All 1,351 captured flags belong to deleted-game cohorts consistent with lifecycle-load signatures, and every one had exactly one capturer. Of 15,814 deleted-game cohort checker rows, 15,528 (98.2%) were NULL-credit InternalError placeholders; attacked cohorts had zero positive SLA observations. Observed 5/8/60/250/300-team topologies anchor scaling checks, but the simulator consumes no raw database rows. Reproduce the scoped aggregates and orphan exclusions with \`historical-audit.sql\`.

The stochastic profiles, exploit diffusion, patch hazards, and availability are synthetic assumptions. They test failure modes and sensitivity, not real player behavior.

## Production Parity

\`manual-equal-balanced\` mirrors the deployed official evidence and formula boundary. The simulator keeps one stable team count for every round, matching the roster frozen from \`startRound\`, and gives every frozen opponent the same denominator for an offense-eligible flag. Positive simulated checker credit stands in for an exact healthy custom-check result; an accepted frozen-roster capture can independently qualify offense reachability. \`rsctf-legacy-gamma-0\` is retained only as the retired fixed-pot comparison. The simulator does not model delayed submission across rsctf's five-tick flag lifetime, so live finalization timing remains qualitative.

The SLA grid is frozen service x scoring round. Offline/Mumble earns zero, the immediately following OK earns 0.5, and a clean OK earns 1. A missing check row is zero rather than a smaller denominator. \`InternalError\` carries the last scored non-infrastructure credit and effective status after \`startRound\`. An isolated first \`InternalError\` earns zero for that service. Only when every frozen service for a challenge-round has a first \`InternalError\` is the sample void for the full roster as a field-wide checker outage.

## Cross-Model Comparison Protocol

Native scores are intentionally not compared: the legacy observational control is approximately \`U+ATK+U*DEF\`, while epoch models are bounded 0-100 scores with whole-score SLA multiplication. Cross-model conclusions use paired ranks, rank correlation, top-three overlap, winner flips, comeback rates, and within-model SLA retention. Rank discards margins and can exaggerate near-tie changes; these diagnostics are sensitivity evidence, not a fairness proof.

${markdownTable(results.slaResponse.rows, slaColumns)}

The exclusive-attacker rows expose the policy difference: the official model and arithmetic control retain exactly \`U*100%\` of their own healthy score, while the legacy observational model retains accepted ATK even at \`U=0\`. The synthetic rows contain no infrastructure faults; production applies the frozen-grid \`InternalError\` policy above rather than treating the verdict as team downtime.

## Copying And Collusion Stress

These 20-team controls compare one capturer (\`k=1\`) with a second capturer and then all eligible opponents (\`k=M=19\`). Retained is the original pioneer's score. Coalition gain is total attacker score after all teams submit divided by the one-attacker control. Victim damage uses a matched counterfactual where the same attackers remain active against other targets; victim/attack is one capture's negative defense damage divided by its attack reward. Both maturity rows are evaluated with every eligible opponent in \`Q_v\`, so they show the fully mature ceiling rather than the early default. For positive DEF, the negative victim-damage fields are \`n/a\`; "Defended +DEF" is the healthy missed target's gain, and competitive issuance is \`ATK+DEF\` in that matched control. "% old ATK" compares it with maturity-negative \`0.45\` ATK on the identical round and context.

${markdownTable(results.deterministic, metricColumns)}

The retired fixed-pot rarity model keeps coalition payout and victim loss fixed, but a pioneer's score falls 50% at the second capture and 94.74% under field-wide AI diffusion. Square-root rarity softens, but does not remove, that effect. Bounded scarcity is the middle negative-defense control: at 20 teams \`k=2\` retains 97.3% of the pioneer's sole-capture score and field-wide copying retains 51.35%. The official model and arithmetic control instead give every accepted capturer base coverage and only a bounded \`(M-k)/M\` premium. Their pairwise defense falls by \`1/M\` per distinct capturer, so one bypass cannot erase the entire flag's DEF.

The explicit funnel control has ${results.collusionFunnelControl.directedMisses} attackers capture every peer except one ally. The legacy observational model awards that ally ${results.collusionFunnelControl.observationalDefense} DEF (${results.collusionFunnelControl.observationalTotal} including quality). Manual arithmetic/balanced award ${results.collusionFunnelControl.manualArithmeticDefense}/${results.collusionFunnelControl.manualBalancedDefense} DEF and totals ${results.collusionFunnelControl.manualArithmeticTotal}/${results.collusionFunnelControl.manualBalancedTotal}, because all of the ally's pairwise outcomes remain protected. This is the known observational weakness: coordinated withholding can inflate DEF, and accepted-flag data alone cannot prove an attempt.

## Maturity Gate And Negative Control

For this maturity-negative control, \`M=19\`, \`k=1\`, and supplied victim-relative \`Q_v\` fixes \`A\`. At \`A<=4\`, \`w=0\` and its \`0.45\` attack share is exactly normalized no-rarity coverage. The smooth ramp starts at five active attackers and reaches full weight at eight; the share continues increasing with \`A\` and equals bounded scarcity only when \`A=M\`. The observational positive-defense control uses the identical \`w(A)\` multiplier with a \`0.30\` attack weight and no victim debit.

${markdownTable(results.maturityControls, maturityControlColumns)}

Qualification requires two distinct other victims and two distinct mint ticks; neither a one-tick sweep nor repeatedly farming one victim is enough. This raises the cost of manufacturing maturity, but does not remove the incentive to coordinate cheap valid captures across targets.

## Field-Size Scaling

Each cell is an exclusive full-field sweep's attack score divided by one healthy additive quality tick. Multiplicative manual models are intentionally omitted because they have no additive quality denominator. The invariance requirement is that the ratio remains O(1) as team count grows. Normalized coverage is exactly 0.45 at every field size. Both maturity rows supply every eligible opponent in \`Q_v\`: the negative row equals 0.8763 at 20 teams and approaches 0.9, while the observational positive row is exactly two-thirds of it, equals 0.5842 at 20 teams, and approaches 0.6. The bounded control intentionally uses a 40/25 offense-to-direct-quality ratio, so its stable target is 1.6.

${markdownTable(scalingRows, scalingColumns)}

The power-law conserved family is \`X=P*(k/M)^gamma\`, where \`X\` is transferred score, \`k\` is capturers, and \`M=N-1\` is eligible opponents frozen when the flag is minted. A sole attacker sweeping the field earns \`P*M^(1-gamma)\`. Against the retired additive SLA scale \`P*sqrt(N)\`, \`gamma=0.5\` is the unique scale-matched exponent in that legacy family. It remains a sensitivity comparator; the official model and arithmetic control normalize accepted attack opportunities and pairwise protected defense opportunities independently.

## Named Campaigns

The early-farmer/late-adapter control gives team 0 identical sweeps in the first two epochs and team 1 identical sweeps in the last two. Equal epochs must tie; positive deltas under recency show the recency rule alone creates a late bonus. The defense/SLA control has one active attacker, one target that remains uncaptured, and peers that are captured; "uncaptured advantage" is observable non-capture, not proof that the attacker tried that target or that a patch blocked it. Values are each formula's native units and must not be compared across rows.

${markdownTable(results.campaigns, scenarioColumns)}

The manual models award rarity only when \`M>=4\`. Every accepted capturer receives rarity fraction \`(M-k)/M\`; the coefficient can add at most 25% of base capture coverage, and the realized absolute lift in \`A\` is at most 20 percentage points. Capturer count is a behavioral proxy, not proof of patch causality. Uncaptured pairwise outcomes likewise record no attempt causality. Start with equal epochs; recency is a separate sensitivity and must not be presented as exploit quality.

## Seeded 20-Team Leaderboard

This is one reproducible campaign scored with \`${results.sampleStandings.formula}\` and equal epochs, not a prediction. It exposes every team's epoch score, first-epoch rank, and final rank so comeback behavior is inspectable rather than inferred from aggregate win rates.

${markdownTable(results.sampleStandings.teams, sampleColumns)}

The deterministic comeback control proves the boundary case with the same scorer: team ${results.lastToFirstControl.team} is rank ${results.lastToFirstControl.epochOneRank} at ${results.lastToFirstControl.epochScores[0]}, then scores ${results.lastToFirstControl.epochScores.slice(1).join(', ')} and finishes rank ${results.lastToFirstControl.finalRank} at settled/projected ${results.lastToFirstControl.settledTotal}/${results.lastToFirstControl.projectedTotal}; the best opponent settles at ${results.lastToFirstControl.bestOpponentSettledTotal}. This proves possibility, not likelihood.

The live-total control uses epoch values ${results.liveEpochTotalControl.epochValues.join(' and ')} with tick counts ${results.liveEpochTotalControl.epochTickCounts.join(' and ')} out of ${results.liveEpochTotalControl.configuredTicksPerEpoch}. During play, with only the first epoch finalized, settled/projected are ${results.liveEpochTotalControl.duringPlay.settledTotal}/${results.liveEpochTotalControl.duringPlay.projectedTotal}. Game end finalizes the \`1/8\` tail without promoting it to full weight, producing ${results.liveEpochTotalControl.atGameEnd.settledTotal}/${results.liveEpochTotalControl.atGameEnd.projectedTotal}.

![Stakeholder view of official EpochBalanced A&D scoring](graphs/synthetic-20-team-epoch-comebacks.png)

## Correlated AI Counterfactual

${results.metadata.trials} seeded trials use ${results.metadata.teamCount} teams, ${results.metadata.epochs} epochs, and ${results.metadata.ticksPerEpoch} ticks per epoch. Five exploit paths per trial are released over time. Paths are independently discovered, then become easier for AI-enabled teams to reproduce after a delay; targets patch each path after observed exposure. This produces correlated captures rather than independent attacker-victim coin flips.

Across the synthetic trials, ${diagnostics.discoveredPathFraction} of exploit paths were discovered, mean discovery delay was ${diagnostics.meanDiscoveryDelayTicks} ticks, captured flags averaged ${diagnostics.meanCapturersPerCapturedFlag} capturers, ${diagnostics.multiCapturerFlagFraction} had multiple capturers, and ${diagnostics.massDiffusionFlagFraction} reached at least half the eligible field. The manual scorer observed ${diagnostics.attackEligibleFlags} offense-eligible flags and ${diagnostics.defenseEligibleFlags} exact-healthy defense flags, ${diagnostics.attackOpportunities} attack opportunities with coverage ${diagnostics.attackCoverage}, and ${diagnostics.protectedDefenseOpportunities}/${diagnostics.defenseOpportunities} protected defense opportunities (${diagnostics.protectedDefenseFraction}). Mean rarity fraction per accepted capture was ${diagnostics.meanRarityFractionPerAcceptedCapture}. Those diagnostics are model outputs, not historical estimates. The generator permits fresh captures only on an OK synthetic target, while the scorer also preserves any accepted capture as offense evidence when checker eligibility is false.

${markdownTable(diagnostics.maturityByEpoch, maturityDiagnosticColumns)}

\`Q_v\` belongs only to the legacy maturity controls. Mean active capturers is measured on captured flags and includes their current capturers. The default/full fractions show how often those controls used their no-rarity ATK floor or full multiplier; they are not historical estimates. The deployed official policy does not consume \`Q_v\`.

${markdownTable(monteRows, monteColumns)}

Under equal epochs, manual-balanced produces fast-agent win share ${manualBalancedEqual.winnerShare.fastAgent}, ${manualBalancedEqual.earlyFastLeads} early fast-over-adaptive leads, and reversal rate ${manualBalancedEqual.adaptiveComebackRate}. Manual-arithmetic produces ${manualArithmeticEqual.winnerShare.fastAgent}, ${manualArithmeticEqual.earlyFastLeads}, and ${manualArithmeticEqual.adaptiveComebackRate}; observational positive DEF produces ${positiveDefenseEqual.winnerShare.fastAgent}, ${positiveDefenseEqual.earlyFastLeads}, and ${positiveDefenseEqual.adaptiveComebackRate}. These are outcomes of the synthetic profiles, not estimates of real win probabilities.

The following paired diagnostics compare each manual model with \`maturity-positive-defense\` on the same 1,000 generated campaigns. Winner flip is a fraction; rank correlation and top-three overlap are unitless.

${markdownTable(pairedRows, pairedColumns)}

\`moderate-recency\` is \`70% mean(all epochs) + 30% mean(last half)\`; \`heavy-recency\` is \`40% + 60%\`. A reversal requires the fast agent to be ahead of the adaptive team after two epochs and the adaptive team to finish ahead. Winner shares include the ${results.metadata.fieldTeamCount} randomized field teams and therefore sum to approximately 1; field/team divides their aggregate share by ${results.metadata.fieldTeamCount}. Adaptive rank score maps average rank onto 100=best and 0=worst so field sizes are comparable. Even with ${results.metadata.trials} trials, a 50% share has about +/-${results.metadata.binomialMarginPct} percentage points of sampling error; structural model uncertainty is much larger. This 20-team run replaces rather than pairs with the old 12-team experiment because the added profile draws change the seeded event stream. Do not select a formula from small table differences.

The 40/35/25 bounded row and the 45/30/25 row are weight-sensitivity controls. Any rank movement between them is evidence that weighted, non-conserved components remain a governance choice requiring event telemetry, not a tuned optimum.

## Operational Policy

1. Use \`EpochBalanced\` (simulator row \`manual-equal-balanced\`) as the sole official A&D ranking and award policy with \`Core=0.4*A+0.4*D+0.2*sqrt(A*D)\`; retain \`manual-equal-arithmetic\` only as the offline \`0.5*A+0.5*D\` governance control.
2. Freeze the ranked team-service roster from flags minted at the global \`startRound\`; reuse it for every epoch and exclude identities absent from that snapshot plus captures attributed to them. Every offense-eligible flag must give all frozen opponents the same opportunity denominator, excluding only the owner.
3. Compute pairwise DEF per exact healthy custom-check flag: \`M\` opportunities and \`M-k\` protected. An accepted capture can independently qualify offense reachability, but must not mint defense. This prevents one bypass from erasing a full flag, while unstolen DEF remains observational.
4. Keep rarity inside \`A\`: when \`M>=4\`, each capturer contributes \`(M-k)/M\` to \`H\`; use \`A=min(1,C+0.25*H)\`. The coefficient adds at most 25% of base coverage and no more than 20 percentage points after clamping. Capturer count does not prove a patch bypass.
5. Reject fallback TCP probes for defense eligibility. Apply SLA locally and linearly to the entire team-service score so one healthy service cannot subsidize another.
6. Build SLA on the frozen service x scoring-round grid. Score a missing check as zero. On \`InternalError\`, carry the last scored non-infrastructure credit/status after \`startRound\`. Score an isolated first error as zero; void the challenge-round sample only when every frozen service has a first error, identifying a field-wide checker outage.
7. Precommit service weights in \`[0.8,1.2]\`, snapshot them per flag/round, and normalize them into one fixed 100-point epoch budget. Use them only for modest operator-set sloppability or difficulty, never as dynamic rarity.
8. Give complete epochs weight 1 and a live/final partial tail weight \`r/n\`. Finalize during play only after the last flag lifetime closes; game end closes the tail without changing its fractional weight. Publish weighted \`settledTotal\` from finalized epochs and weighted \`projectedTotal\` from all current evidence. Keep only the latest three epoch detail rows in the list UI while both totals use all epochs. The first complete roster with prepared exact custom checkers starts scoring automatically at the single locked \`startRound\`; exclude all earlier evidence.
9. Treat flag sharing, sybils, deliberate non-submission, and target-aware special casing as rules and telemetry problems; outcome scoring cannot prove independent discovery or attempted exploitation.

## Reproduction

~~~sh
node tools/ad-scoring-sim/test.mjs
node tools/ad-scoring-sim/simulate.mjs
node tools/ad-scoring-sim/simulate.mjs --check
psql "$RSCTF_DATABASE_URL" -X -v ON_ERROR_STOP=1 \\
  -f tools/ad-scoring-sim/historical-audit.sql
~~~

Machine-readable results are in \`tools/ad-scoring-sim/results.json\`. Primary format references: [OtterSec Save CTFs Fund](https://osec.io/blog/save-ctfs-fund/), [ECSC 2025](https://wiki.ad.ecsc2025.pl/scoring/), [FAUST CTF 2025 rules](https://2025.faustctf.net/information/rules/), and the [AIxCC scoring guide](https://aicyberchallenge.com/storage/2025/06/AFC-Procedures-and-Scoring-Guide-Version-2_0-_20250606.pdf).
`
}

const deterministic = Object.keys(EVALUATED_SCORERS).map((formula) => formulaMetrics(formula))
const { sampleStandings, sampleComparisons } = runSampleComparisons()
const lastToFirstControl = runLastToFirstControl()
const monteCarlo = runMonteCarlo()
const maturityControlResults = maturityControls()
const collusionControl = collusionFunnelControl()
const slaResponse = buildSlaResponse()
const stakeholderExamples = buildStakeholderExamples()
const results = {
  metadata: {
    resultsSchemaVersion: 5,
    seed: SEED,
    trials: TRIALS,
    teamCount: TEAM_COUNT,
    fieldTeamCount: TEAM_COUNT - 3,
    massDiffusionCapturerThreshold: Math.ceil((TEAM_COUNT - 1) / 2),
    epochs: EPOCHS,
    ticksPerEpoch: TICKS_PER_EPOCH,
    maturityDistinctVictimThreshold: 2,
    maturityDistinctTickThreshold: 2,
    maturityRampStart: 4,
    maturityRampEnd: 8,
    positiveDefenseAttackWeight: 0.30,
    positiveDefenseRatio: 0.5,
    positiveDefenseSlaWeight: 1,
    manualEpochBudget: 100,
    manualArithmeticWeights: { attack: 0.5, defense: 0.5, balance: 0 },
    manualBalancedWeights: { attack: 0.4, defense: 0.4, balance: 0.2 },
    deployedOfficialFormula: FORMULAS.manualBalanced,
    officialScoringMode: 'EpochBalanced',
    officialBoardDeterminesRankAndAwards: true,
    manualRarityCoefficient: 0.25,
    manualRarityMinOpponents: 4,
    serviceWeightBounds: [0.8, 1.2],
    simulatedServiceWeight: 1,
    serviceWeightIsPrecommittedAndSnapshotted: true,
    scoringRosterFrozenAtStartRound: true,
    outsideFrozenRosterCapturesExcluded: true,
    attackDenominatorSharedByFrozenRoster: true,
    acceptedCaptureQualifiesOffenseFlag: true,
    defenseRequiresExactHealthyCustomCheck: true,
    missingCheckCredit: 0,
    internalErrorPolicy: 'carry prior; isolated first error is zero; all-services first error voids challenge-round',
    manualScoringStartRound: 1,
    midGameEnablementStartsNextRound: true,
    manualDefenseIsPairwise: true,
    manualDefenseIsObservational: true,
    manualSlaMultipliesWholeScore: true,
    defenseEligibilityRequiresHealthyCustomChecker: true,
    fallbackTcpProbeQualifiesDefense: false,
    liveTotals: {
      settled: 'finalized epochs only',
      projected: 'all current epoch evidence',
      gameEndFinalizesPartialTail: true,
      fullEpochWeight: 1,
      partialTailWeight: 'observed ticks / configured ticks',
    },
    epochDetailRowsRetainedInListUi: 3,
    totalsUseAllEpochs: true,
    exploitPathsPerTrial: 5,
    syntheticGeneratorFreshCaptureRequiresOkStatus: true,
    delayedSubmissionsModeled: false,
    historicalFieldSizes: HISTORICAL_FIELD_SIZES,
    evaluatedFieldSizes: FIELD_SIZES,
    binomialMarginPct: roundNumber(100 * 1.96 * Math.sqrt(0.25 / TRIALS), 1),
  },
  historicalEvidence: {
    auditSnapshotUtc: '2026-07-10T22:45:34Z',
    auditQuery: 'tools/ad-scoring-sim/historical-audit.sql',
    survivingGameAttacks: 0,
    deletedGameCohorts: 36,
    attackedDeletedGameCohorts: 24,
    historicalAttacks: 1351,
    capturerMultiplicity: { one: 1351, twoOrMore: 0 },
    deletedGameCheckerRows: 15814,
    deletedGameInternalErrorRows: 15528,
    deletedGameInternalErrorFraction: 0.98191,
    attackedCohortCheckerRows: 14986,
    attackedCohortPositiveSlaRows: 0,
    burstCohortsAtMostOneSecond: { count: 23, total: 24 },
    historicalAttackRowsWithAttackerParticipation: 0,
    consumesRawDatabaseRows: false,
    note: 'Deleted-game topology and burst signatures are consistent with lifecycle load traffic, not player evidence.',
  },
  formulaDefinitions: {
    current: 'Retired fixed rarity pot: each capturer gets P/k; victim loses P.',
    sqrt: 'Conserved coverage transfer X=P*sqrt(k/M), split X across k capturers.',
    flat: 'No-rarity conserved transfer X=P*k/M, so every capturer gets P/M.',
    normalized: 'No-rarity conserved coverage: ATK=0.45*targets/M, DEF=-0.45*capturers/M, SLA=q.',
    scarcity: 'Bounded scarcity transfer X=0.45*(k/M)*(2-k/M), split across k capturers; victim loses X.',
    maturity: 'Maturity-gated scarcity share=(0.45/M)*[1+w(A)*(1-k/A)], A=|C union Q_v|; victim loses k*share.',
    positiveDefense: 'Maturity ATK uses weight 0.30 with no victim debit; each active attacker funds up to 0.5*ATK across healthy missed targets, and uptime scales DEF.',
    ictf: 'iCTF-like fixed service pot comparison.',
    matrix: 'Active-attacker pair pots; non-capture is not proof of an attempted block.',
    bounded: '40% normalized coverage + 35% remaining defense coverage + 25% SLA credit.',
    boundedOffenseHeavy: '45% normalized coverage + 30% remaining defense coverage + 25% SLA credit.',
    manualArithmetic: 'Offline governance control: Core=0.5*A+0.5*D; accepted flags set A, pairwise protected opportunities set D, and local SLA R multiplies the whole fixed-budget score.',
    manualBalanced: 'Deployed official EpochBalanced policy: Core=0.4*A+0.4*D+0.2*sqrt(A*D); accepted flags set A, pairwise protected opportunities set D, and local SLA R multiplies the whole fixed-budget score.',
  },
  scenarioDefinitions: {
    commonAiExploit: 'One flag moves from one capturer to every eligible capturer.',
    lateExclusiveProxy: 'A sole late capturer is compared with a field-wide common path without asserting patch causality.',
    earlyFarmerVsLateAdapter: 'Identical exclusive sweeps occur in the first two versus last two epochs.',
    defenseSla: 'An active attacker captures peers while one healthy target remains uncaptured; this is observational, not proof of an attempt.',
    collusionControl: 'One valid capture is compared with the same flag submitted by the full eligible field.',
  },
  maturityControls: maturityControlResults,
  collusionFunnelControl: collusionControl,
  deterministic,
  fieldScaling: fieldScaling(),
  campaigns: deterministicCampaigns(),
  sampleStandings,
  sampleComparisons,
  lastToFirstControl,
  liveEpochTotalControl: liveEpochTotalControl(),
  slaResponse,
  stakeholderExamples,
  pairedComparisons: monteCarlo.pairedComparisons,
  monteCarlo,
}

const directory = path.dirname(fileURLToPath(import.meta.url))
const generatedFiles = [
  [path.join(directory, 'results.json'), `${JSON.stringify(results, null, 2)}\n`],
  [path.join(directory, 'REPORT.md'), buildReport(results)],
]

if (process.argv.includes('--check')) {
  for (const [file, expected] of generatedFiles) {
    if (!fs.existsSync(file) || fs.readFileSync(file, 'utf8') !== expected) {
      throw new Error(`stale generated file: ${file}; run simulate.mjs without --check`)
    }
  }
  console.log('ad scoring simulator generated files: current')
} else {
  for (const [file, contents] of generatedFiles) {
    fs.writeFileSync(file, contents)
    console.log(`wrote ${file}`)
  }
}
