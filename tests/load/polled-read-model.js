// Walk each complete endpoint cycle before moving to the next cohort member.
// Multiplying the sequence by a stride can alias with common cohort sizes.
export function polledReadSelection(iteration, endpointCount, tokenCount) {
  if (!Number.isSafeInteger(iteration) || iteration < 0 ||
      !Number.isSafeInteger(endpointCount) || endpointCount < 1 ||
      !Number.isSafeInteger(tokenCount) || tokenCount < 1) {
    throw new Error('polling selection requires a nonnegative iteration and positive sizes');
  }
  return {
    endpointIndex: iteration % endpointCount,
    tokenIndex: Math.floor(iteration / endpointCount) % tokenCount,
  };
}
