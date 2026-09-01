import { useState, useEffect } from 'react'
import { OnceSWRConfig } from '@Hooks/useConfig'
import api, { ChallengeInfoModel } from '@Api'

export const useEditChallenge = (numId: number, numCId: number) => {
  const { data: challenge, error, mutate, isValidating } = api.edit.useEditGetGameChallenge(
    numId,
    numCId,
    OnceSWRConfig
  )

  return { challenge, error, mutate, isValidating }
}

export const useEditChallenges = (numId: number) => {
  const { data, error, mutate, isValidating } = api.edit.useEditGetGameChallenges(numId, OnceSWRConfig)

  const [sortedChallenges, setSortedChallenges] = useState<ChallengeInfoModel[] | null>(null)

  useEffect(() => {
    if (data) {
      setSortedChallenges(data.toSorted((a, b) => ((a.category ?? '') > (b.category ?? '') ? -1 : 1)))
    }
  }, [data])

  return { challenges: sortedChallenges, error, mutate, isValidating }
}
