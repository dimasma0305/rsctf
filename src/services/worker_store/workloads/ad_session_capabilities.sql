(
    workload.spec ->> 'gameKind' IS DISTINCT FROM 'attackDefense'
    OR (
        (workload.desired_state = 'Absent'
         OR node.capabilities @> '{"verifiedFlagDelivery":true}'::jsonb)
        AND (workload.spec ->> 'teamDataMount' IS NULL
             OR node.capabilities @> '{"teamDataMount":true}'::jsonb)
    )
)
